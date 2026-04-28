//! Background reaper task (Phase 3, commit 5/13).
//!
//! Tokio task spawned at server startup. Every
//! [`REAPER_INTERVAL_SECS`] (30s default) it scans for two classes
//! of expired jobs and transitions them to `errored`:
//!
//! 1. **Dispatch deadline expired** — runners that accepted a
//!    dispatch (replied 2xx) but never emitted a `Started` event
//!    within `dispatch_deadline_at` (10s post-dispatch). Marks
//!    `error_stage = "dispatch"`.
//! 2. **Lease expired** — in-progress runners that haven't emitted
//!    a heartbeat / progress event within `lease_expires_at` (5
//!    min, extended on every progress event). Marks
//!    `error_stage = "lease_expired"`.
//!
//! State transitions go through
//! [`Store::advance_replay_job_state`] which has the SQL-level
//! terminal-state guard from review #152 — the reaper can't race
//! a late `Completed` event into corrupted state.
//!
//! ## Observability
//!
//! Each transition logs an `INFO` with the job id, runner id, and
//! reason. Operators tailing logs see lease/dispatch failures live.
//! The dashboard's WebSocket also broadcasts the `errored` event
//! (commit 6 wires the broadcaster).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rewind_store::ReplayJobState;
use tokio::sync::broadcast;

use crate::{AppState, StoreEvent};

/// How often the reaper scans. 30 seconds gives a worst-case 30s
/// detection lag on top of the 10s dispatch deadline / 5min lease,
/// which is acceptable for human-visible UX.
pub const REAPER_INTERVAL_SECS: u64 = 30;

/// Spawn the reaper background task. Returns the JoinHandle so
/// shutdown / tests can await it (production code drops it).
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(REAPER_INTERVAL_SECS));
        // First tick fires immediately; we want a delay so the
        // server is fully up before the first scan.
        interval.tick().await;
        loop {
            interval.tick().await;
            tick(&state);
        }
    })
}

/// Single reaper tick. Public for tests and integration testing.
pub fn tick(state: &AppState) -> ReaperTickStats {
    let now = Utc::now();
    let mut stats = ReaperTickStats::default();

    let store = match state.store.lock() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("reaper: store lock poisoned: {e}");
            return stats;
        }
    };

    // 1. Dispatch-deadline expired.
    match store.list_dispatch_deadline_expired(now) {
        Ok(jobs) => {
            for job in jobs {
                let advanced = store.advance_replay_job_state(
                    &job.id,
                    ReplayJobState::Errored,
                    Some("runner did not start within 10s of dispatch"),
                    Some("dispatch"),
                );
                match advanced {
                    Ok(true) => {
                        stats.dispatch_deadline_expired += 1;
                        tracing::info!(
                            "reaper: job {} (runner {:?}) → errored (dispatch deadline)",
                            job.id, job.runner_id
                        );
                        broadcast_replay_job_errored(
                            &state.event_tx,
                            &job.id,
                            &job.session_id,
                            "dispatch",
                            "runner did not start within 10s of dispatch",
                        );
                    }
                    Ok(false) => {
                        // Already terminal — race with a late event.
                        // SQL guard won; nothing to do.
                    }
                    Err(e) => {
                        tracing::error!(
                            "reaper: failed to mark job {} errored (dispatch): {e}",
                            job.id
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!("reaper: list_dispatch_deadline_expired failed: {e}");
        }
    }

    // 2. Lease expired (runner stopped emitting events mid-run).
    let _ = now; // current Store API uses Utc::now() internally; param kept for symmetry
    match store.list_expired_replay_jobs() {
        Ok(jobs) => {
            for job in jobs {
                // list_expired_jobs returns BOTH dispatched and
                // in_progress jobs whose lease expired. We've already
                // handled the dispatched-without-Started case above
                // via list_dispatch_deadline_expired. The lease-only
                // case is in_progress jobs whose heartbeats stopped.
                if !matches!(job.state, ReplayJobState::InProgress) {
                    continue;
                }
                let advanced = store.advance_replay_job_state(
                    &job.id,
                    ReplayJobState::Errored,
                    Some("runner heartbeat lease expired (no progress for 5 min)"),
                    Some("lease_expired"),
                );
                match advanced {
                    Ok(true) => {
                        stats.lease_expired += 1;
                        tracing::info!(
                            "reaper: job {} (runner {:?}) → errored (lease expired)",
                            job.id, job.runner_id
                        );
                        broadcast_replay_job_errored(
                            &state.event_tx,
                            &job.id,
                            &job.session_id,
                            "lease_expired",
                            "runner heartbeat lease expired (no progress for 5 min)",
                        );
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::error!(
                            "reaper: failed to mark job {} errored (lease): {e}",
                            job.id
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!("reaper: list_expired_jobs failed: {e}");
        }
    }

    stats
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReaperTickStats {
    pub dispatch_deadline_expired: u32,
    pub lease_expired: u32,
}

fn broadcast_replay_job_errored(
    tx: &broadcast::Sender<StoreEvent>,
    job_id: &str,
    session_id: &str,
    error_stage: &str,
    error_message: &str,
) {
    let _ = tx.send(StoreEvent::ReplayJobUpdated {
        job_id: job_id.to_string(),
        session_id: session_id.to_string(),
        state: "errored".to_string(),
        progress_step: None,
        progress_total: None,
        error_message: Some(error_message.to_string()),
        error_stage: Some(error_stage.to_string()),
    });
}

// `Arc` import suppression for the case where AppState's store field
// holds Arc<Mutex<Store>>; not directly used here but keeps the type
// signature documentation honest.
#[allow(dead_code)]
fn _arc_marker(_: Arc<()>) {}

// ──────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use rewind_store::{ReplayJob, ReplayJobState, Runner, RunnerMode, RunnerStatus, Session, Store, Timeline};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    fn fixture_state() -> (AppState, Arc<Mutex<Store>>, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let store = Arc::new(Mutex::new(store));
        let (event_tx, _) = broadcast::channel::<StoreEvent>(64);
        let state = AppState {
            store: store.clone(),
            event_tx,
            hooks: Arc::new(crate::HookIngestionState::new()),
            otel_config: None,
            auth_token: None,
            crypto: None, dispatcher: None, base_url: "http://127.0.0.1:4800".to_string(),
        };
        (state, store, tmp)
    }

    fn seed_runner(store: &Store) -> Runner {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"reaper-token");
        let runner = Runner {
            id: Uuid::new_v4().to_string(),
            name: "reaper-test".into(),
            mode: RunnerMode::Webhook,
            webhook_url: Some("http://1.1.1.1/wh".into()),
            encrypted_token: vec![1, 2, 3],
            token_nonce: vec![0; 12],
            auth_token_hash: format!("{:x}", h.finalize()),
            auth_token_preview: "tok***".into(),
            created_at: Utc::now(),
            last_seen_at: None,
            status: RunnerStatus::Active,
        };
        store.create_runner(&runner).unwrap();
        runner
    }

    fn seed_session_and_ctx(store: &Store) -> (String, String) {
        let session = Session::new("reaper-session");
        let session_id = session.id.clone();
        let timeline = Timeline::new_root(&session_id);
        store.create_session(&session).unwrap();
        store.create_timeline(&timeline).unwrap();
        let ctx_id = Uuid::new_v4().to_string();
        store
            .create_replay_context(&ctx_id, &session_id, &timeline.id, 0)
            .unwrap();
        (session_id, ctx_id)
    }

    fn make_dispatched_job(
        runner_id: &str,
        session_id: &str,
        ctx_id: &str,
        deadline_offset: ChronoDuration,
    ) -> ReplayJob {
        let now = Utc::now();
        ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id.into()),
            session_id: session_id.into(),
            replay_context_id: Some(ctx_id.into()),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: now,
            dispatched_at: Some(now),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(now + deadline_offset),
            lease_expires_at: Some(now + ChronoDuration::seconds(300)),
            progress_step: 0,
            progress_total: None,
        }
    }

    #[test]
    fn tick_marks_dispatch_deadline_expired_jobs() {
        let (state, store_arc, _tmp) = fixture_state();
        let store = store_arc.lock().unwrap();
        let runner = seed_runner(&store);
        let (session_id, ctx_id) = seed_session_and_ctx(&store);
        // Deadline 10s in the past → already expired.
        let job = make_dispatched_job(
            &runner.id,
            &session_id,
            &ctx_id,
            ChronoDuration::seconds(-10),
        );
        let job_id = job.id.clone();
        store.create_replay_job(&job).unwrap();
        drop(store);

        let stats = tick(&state);
        assert_eq!(stats.dispatch_deadline_expired, 1);
        assert_eq!(stats.lease_expired, 0);

        let store = store_arc.lock().unwrap();
        let after = store.get_replay_job(&job_id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(after.error_stage.as_deref(), Some("dispatch"));
    }

    #[test]
    fn tick_does_not_touch_dispatched_jobs_within_deadline() {
        let (state, store_arc, _tmp) = fixture_state();
        let store = store_arc.lock().unwrap();
        let runner = seed_runner(&store);
        let (session_id, ctx_id) = seed_session_and_ctx(&store);
        let job = make_dispatched_job(
            &runner.id,
            &session_id,
            &ctx_id,
            ChronoDuration::seconds(60), // future deadline
        );
        let job_id = job.id.clone();
        store.create_replay_job(&job).unwrap();
        drop(store);

        let stats = tick(&state);
        assert_eq!(stats.dispatch_deadline_expired, 0);

        let store = store_arc.lock().unwrap();
        let after = store.get_replay_job(&job_id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Dispatched);
    }

    #[test]
    fn tick_marks_in_progress_jobs_with_expired_lease() {
        let (state, store_arc, _tmp) = fixture_state();
        let store = store_arc.lock().unwrap();
        let runner = seed_runner(&store);
        let (session_id, ctx_id) = seed_session_and_ctx(&store);
        let now = Utc::now();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner.id.clone()),
            session_id: session_id.clone(),
            replay_context_id: Some(ctx_id.clone()),
            state: ReplayJobState::InProgress,
            error_message: None,
            error_stage: None,
            created_at: now,
            dispatched_at: Some(now),
            started_at: Some(now),
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: Some(now - ChronoDuration::seconds(60)), // expired
            progress_step: 3,
            progress_total: Some(10),
        };
        let job_id = job.id.clone();
        store.create_replay_job(&job).unwrap();
        drop(store);

        let stats = tick(&state);
        assert_eq!(stats.lease_expired, 1);

        let store = store_arc.lock().unwrap();
        let after = store.get_replay_job(&job_id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(after.error_stage.as_deref(), Some("lease_expired"));
    }
}
