//! Replay-job tracking (extracted from the former runner registry module).
//!
//! Two tables, three Rust types:
//!
//! - **`replay_jobs`** <-> [`ReplayJob`]: dispatched replay jobs going
//!   through the state machine `pending -> dispatched -> in_progress ->
//!   completed/errored`. Includes lease columns (`dispatch_deadline_at`,
//!   `lease_expires_at`) for the reaper task.
//! - **`replay_job_events`** <-> [`ReplayJobEvent`]: append-only event log
//!   per job (started/progress/completed/errored). The dashboard's
//!   WebSocket re-broadcasts these.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::Store;

// -- ReplayJob ----------------------------------------------------------------

/// State machine: `pending -> dispatched -> in_progress -> completed/errored`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayJobState {
    Pending,
    Dispatched,
    InProgress,
    Completed,
    Errored,
}

impl ReplayJobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReplayJobState::Pending => "pending",
            ReplayJobState::Dispatched => "dispatched",
            ReplayJobState::InProgress => "in_progress",
            ReplayJobState::Completed => "completed",
            ReplayJobState::Errored => "errored",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(ReplayJobState::Pending),
            "dispatched" => Some(ReplayJobState::Dispatched),
            "in_progress" => Some(ReplayJobState::InProgress),
            "completed" => Some(ReplayJobState::Completed),
            "errored" => Some(ReplayJobState::Errored),
            _ => None,
        }
    }
    pub fn is_terminal(&self) -> bool {
        matches!(self, ReplayJobState::Completed | ReplayJobState::Errored)
    }
}

/// A dispatched replay job. Tracks state, lease deadlines, and progress.
///
/// `runner_id` is kept as `Option<String>` for backward compatibility with
/// existing databases that still have the column populated from the old
/// runner registry. New jobs always set it to `None`.
///
/// `dispatch_token` is a random nonce generated when the job is created and
/// sent in the webhook payload. The runner must echo it back via the
/// `X-Rewind-Dispatch-Token` header when posting events — this proves the
/// caller received the original dispatch without requiring key management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayJob {
    pub id: String,
    pub runner_id: Option<String>,
    pub session_id: String,
    pub replay_context_id: Option<String>,
    pub state: ReplayJobState,
    pub error_message: Option<String>,
    pub error_stage: Option<String>,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub dispatch_deadline_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub progress_step: u32,
    pub progress_total: Option<u32>,
    pub dispatch_token: Option<String>,
}

// -- ReplayJobEvent -----------------------------------------------------------

/// A single event emitted during job execution. Append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayJobEventType {
    Started,
    Progress,
    Completed,
    Errored,
}

impl ReplayJobEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReplayJobEventType::Started => "started",
            ReplayJobEventType::Progress => "progress",
            ReplayJobEventType::Completed => "completed",
            ReplayJobEventType::Errored => "errored",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "started" => Some(ReplayJobEventType::Started),
            "progress" => Some(ReplayJobEventType::Progress),
            "completed" => Some(ReplayJobEventType::Completed),
            "errored" => Some(ReplayJobEventType::Errored),
            _ => None,
        }
    }
}

/// One row from `replay_job_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayJobEvent {
    pub id: String,
    pub job_id: String,
    pub event_type: ReplayJobEventType,
    pub step_number: Option<u32>,
    pub payload: Option<String>,
    pub created_at: DateTime<Utc>,
}

// -- CRUD on Store ------------------------------------------------------------

impl Store {
    /// Insert a new replay job. The `runner_id` column is always set to
    /// NULL (the runner registry has been removed).
    pub fn create_replay_job(&self, job: &ReplayJob) -> Result<()> {
        if !job.state.is_terminal() && job.replay_context_id.is_none() {
            return Err(anyhow!(
                "replay_jobs.replay_context_id is required for non-terminal jobs (state={}); null is only allowed for completed/errored historical rows",
                job.state.as_str()
            ));
        }
        self.conn.execute(
            "INSERT INTO replay_jobs (id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total, dispatch_token)
             VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                job.id,
                job.session_id,
                job.replay_context_id,
                job.state.as_str(),
                job.error_message,
                job.error_stage,
                job.created_at.to_rfc3339(),
                job.dispatched_at.as_ref().map(|t| t.to_rfc3339()),
                job.started_at.as_ref().map(|t| t.to_rfc3339()),
                job.completed_at.as_ref().map(|t| t.to_rfc3339()),
                job.dispatch_deadline_at.as_ref().map(|t| t.to_rfc3339()),
                job.lease_expires_at.as_ref().map(|t| t.to_rfc3339()),
                job.progress_step,
                job.progress_total,
                job.dispatch_token,
            ],
        )?;
        Ok(())
    }

    pub fn get_replay_job(&self, id: &str) -> Result<Option<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total, dispatch_token
             FROM replay_jobs WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_replay_job)?;
        Ok(rows.next().transpose()?)
    }

    /// List jobs for a session, newest first.
    pub fn list_replay_jobs_by_session(&self, session_id: &str) -> Result<Vec<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total, dispatch_token
             FROM replay_jobs WHERE session_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![session_id], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Transition a job to a new state.
    ///
    /// Terminal-state protection is enforced at the SQL level via
    /// `WHERE state NOT IN ('completed', 'errored')`.
    ///
    /// Returns `true` if the row was updated (state advanced),
    /// `false` if no row matched (job doesn't exist or already terminal).
    pub fn advance_replay_job_state(
        &self,
        id: &str,
        new_state: ReplayJobState,
        error_message: Option<&str>,
        error_stage: Option<&str>,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let (timestamp_col, timestamp_val): (Option<&str>, Option<&str>) = match new_state {
            ReplayJobState::Dispatched => (Some("dispatched_at"), Some(now.as_str())),
            ReplayJobState::InProgress => (Some("started_at"), Some(now.as_str())),
            ReplayJobState::Completed | ReplayJobState::Errored => {
                (Some("completed_at"), Some(now.as_str()))
            }
            ReplayJobState::Pending => (None, None),
        };

        let n = if let (Some(col), Some(val)) = (timestamp_col, timestamp_val) {
            self.conn.execute(
                &format!(
                    "UPDATE replay_jobs SET state = ?1, error_message = ?2, error_stage = ?3, {col} = ?4
                     WHERE id = ?5 AND state NOT IN ('completed', 'errored')"
                ),
                params![new_state.as_str(), error_message, error_stage, val, id],
            )?
        } else {
            self.conn.execute(
                "UPDATE replay_jobs SET state = ?1, error_message = ?2, error_stage = ?3
                 WHERE id = ?4 AND state NOT IN ('completed', 'errored')",
                params![new_state.as_str(), error_message, error_stage, id],
            )?
        };
        Ok(n > 0)
    }

    /// Mark a `dispatched` job as `errored`. Stricter than
    /// `advance_replay_job_state` because it ONLY matches rows
    /// currently in `dispatched`.
    pub fn mark_dispatched_job_as_errored(
        &self,
        id: &str,
        error_message: &str,
        error_stage: &str,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE replay_jobs
             SET state = 'errored',
                 error_message = ?1,
                 error_stage = ?2,
                 completed_at = ?3
             WHERE id = ?4 AND state = 'dispatched'",
            params![error_message, error_stage, now, id],
        )?;
        Ok(n > 0)
    }

    /// Atomically record an event AND apply the state/progress/lease
    /// update it implies. Single transaction; full state-machine
    /// guarded in SQL.
    pub fn record_replay_job_event_atomic(
        &mut self,
        event: &ReplayJobEvent,
        progress_total: Option<u32>,
        error_message: Option<&str>,
        error_stage: Option<&str>,
        lease_extension_seconds: i64,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;

        let current_state: Option<String> = tx
            .query_row(
                "SELECT state FROM replay_jobs WHERE id = ?1",
                params![event.job_id],
                |row| row.get(0),
            )
            .ok();
        let Some(state_str) = current_state else {
            tx.rollback()?;
            return Ok(false);
        };

        let current_state_enum = ReplayJobState::from_db_str(&state_str).ok_or_else(|| {
            anyhow!(
                "invalid state in DB for job {}: {}",
                event.job_id,
                state_str
            )
        })?;
        let legal = match event.event_type {
            ReplayJobEventType::Started => {
                matches!(current_state_enum, ReplayJobState::Dispatched)
            }
            ReplayJobEventType::Progress => {
                matches!(current_state_enum, ReplayJobState::InProgress)
            }
            ReplayJobEventType::Completed => {
                matches!(current_state_enum, ReplayJobState::InProgress)
            }
            ReplayJobEventType::Errored => matches!(
                current_state_enum,
                ReplayJobState::Dispatched | ReplayJobState::InProgress
            ),
        };
        if !legal {
            tx.rollback()?;
            return Ok(false);
        }

        tx.execute(
            "INSERT INTO replay_job_events (id, job_id, event_type, step_number, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.job_id,
                event.event_type.as_str(),
                event.step_number,
                event.payload,
                event.created_at.to_rfc3339(),
            ],
        )?;

        let now = Utc::now().to_rfc3339();
        let new_lease = (Utc::now() + chrono::Duration::seconds(lease_extension_seconds))
            .to_rfc3339();
        match event.event_type {
            ReplayJobEventType::Started => {
                tx.execute(
                    "UPDATE replay_jobs SET state = 'in_progress', started_at = ?1, lease_expires_at = ?2
                     WHERE id = ?3",
                    params![now, new_lease, event.job_id],
                )?;
            }
            ReplayJobEventType::Progress => {
                tx.execute(
                    "UPDATE replay_jobs SET progress_step = ?1, progress_total = COALESCE(?2, progress_total), lease_expires_at = ?3
                     WHERE id = ?4",
                    params![event.step_number.unwrap_or(0), progress_total, new_lease, event.job_id],
                )?;
            }
            ReplayJobEventType::Completed => {
                tx.execute(
                    "UPDATE replay_jobs SET state = 'completed', completed_at = ?1
                     WHERE id = ?2",
                    params![now, event.job_id],
                )?;
            }
            ReplayJobEventType::Errored => {
                tx.execute(
                    "UPDATE replay_jobs SET state = 'errored', completed_at = ?1, error_message = ?2, error_stage = ?3
                     WHERE id = ?4",
                    params![now, error_message, error_stage, event.job_id],
                )?;
            }
        }

        tx.commit()?;
        Ok(true)
    }

    /// Extend the lease (called on heartbeat / progress events).
    pub fn extend_replay_job_lease(
        &self,
        id: &str,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE replay_jobs SET lease_expires_at = ?1 WHERE id = ?2 AND state IN ('dispatched', 'in_progress')",
            params![lease_expires_at.to_rfc3339(), id],
        )?;
        Ok(())
    }

    /// Update progress counters.
    pub fn update_replay_job_progress(
        &self,
        id: &str,
        step_number: u32,
        progress_total: Option<u32>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE replay_jobs SET progress_step = ?1, progress_total = COALESCE(?2, progress_total) WHERE id = ?3",
            params![step_number, progress_total, id],
        )?;
        Ok(())
    }

    /// Set the dispatch deadline and initial lease on a freshly-dispatched job.
    pub fn set_dispatch_deadline_and_lease(
        &self,
        id: &str,
        dispatch_deadline_at: DateTime<Utc>,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE replay_jobs
             SET dispatch_deadline_at = ?1, lease_expires_at = ?2
             WHERE id = ?3",
            params![
                dispatch_deadline_at.to_rfc3339(),
                lease_expires_at.to_rfc3339(),
                id
            ],
        )?;
        Ok(())
    }

    /// Reaper-side query: jobs in state `dispatched` whose
    /// `dispatch_deadline_at < now`.
    pub fn list_dispatch_deadline_expired(&self, now: DateTime<Utc>) -> Result<Vec<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total, dispatch_token
             FROM replay_jobs
             WHERE state = 'dispatched' AND dispatch_deadline_at IS NOT NULL AND dispatch_deadline_at < ?1
             ORDER BY dispatch_deadline_at ASC LIMIT 1000",
        )?;
        let rows = stmt.query_map(params![now.to_rfc3339()], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find expired jobs (dispatch deadline OR lease expired) that
    /// are still in non-terminal states.
    pub fn list_expired_replay_jobs(&self) -> Result<Vec<ReplayJob>> {
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total, dispatch_token
             FROM replay_jobs
             WHERE state IN ('dispatched', 'in_progress')
               AND (
                   (state = 'dispatched' AND dispatch_deadline_at IS NOT NULL AND dispatch_deadline_at < ?1)
                OR (state = 'in_progress' AND lease_expires_at IS NOT NULL AND lease_expires_at < ?1)
               )",
        )?;
        let rows = stmt.query_map(params![now], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Count non-terminal jobs that reference this replay context.
    pub fn count_in_flight_jobs_for_replay_context(&self, replay_context_id: &str) -> Result<u32> {
        let n: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM replay_jobs
             WHERE replay_context_id = ?1
               AND state NOT IN ('completed', 'errored')",
            params![replay_context_id],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    /// List events for a job in insertion order (created_at asc).
    pub fn list_replay_job_events(&self, job_id: &str) -> Result<Vec<ReplayJobEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job_id, event_type, step_number, payload, created_at
             FROM replay_job_events WHERE job_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![job_id], |row| {
            Ok(ReplayJobEvent {
                id: row.get(0)?,
                job_id: row.get(1)?,
                event_type: {
                    let s: String = row.get(2)?;
                    ReplayJobEventType::from_db_str(&s).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            format!("invalid event type: {s}").into(),
                        )
                    })?
                },
                step_number: row.get(3)?,
                payload: row.get(4)?,
                created_at: parse_dt(row, 5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn row_to_replay_job(row: &rusqlite::Row) -> rusqlite::Result<ReplayJob> {
        Ok(ReplayJob {
            id: row.get(0)?,
            runner_id: row.get(1)?,
            session_id: row.get(2)?,
            replay_context_id: row.get(3)?,
            state: {
                let s: String = row.get(4)?;
                ReplayJobState::from_db_str(&s).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        format!("invalid replay job state: {s}").into(),
                    )
                })?
            },
            error_message: row.get(5)?,
            error_stage: row.get(6)?,
            created_at: parse_dt(row, 7)?,
            dispatched_at: parse_dt_opt(row, 8)?,
            started_at: parse_dt_opt(row, 9)?,
            completed_at: parse_dt_opt(row, 10)?,
            dispatch_deadline_at: parse_dt_opt(row, 11)?,
            lease_expires_at: parse_dt_opt(row, 12)?,
            progress_step: row.get(13)?,
            progress_total: row.get(14)?,
            dispatch_token: row.get(15)?,
        })
    }

    /// Insert an event row without checking job state.
    /// Gated behind `#[cfg(test)]` because it bypasses the terminal-state guard.
    #[cfg(test)]
    pub(crate) fn append_replay_job_event(&self, event: &ReplayJobEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO replay_job_events (id, job_id, event_type, step_number, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.job_id,
                event.event_type.as_str(),
                event.step_number,
                event.payload,
                event.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}

// -- Helpers ------------------------------------------------------------------

fn parse_dt(row: &rusqlite::Row, col: usize) -> rusqlite::Result<DateTime<Utc>> {
    let s: String = row.get(col)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                col,
                rusqlite::types::Type::Text,
                format!("bad datetime in column {col}: {e}").into(),
            )
        })
}

fn parse_dt_opt(row: &rusqlite::Row, col: usize) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let s: Option<String> = row.get(col)?;
    s.map(|s| {
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    col,
                    rusqlite::types::Type::Text,
                    format!("bad datetime in column {col}: {e}").into(),
                )
            })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (store, dir)
    }

    fn fake_replay_context_for_job(store: &Store, session_id: &str) -> String {
        let timeline_id = Uuid::new_v4().to_string();
        store
            .conn
            .execute(
                "INSERT INTO timelines (id, session_id, parent_timeline_id, fork_at_step, created_at, label)
                 VALUES (?1, ?2, NULL, NULL, ?3, 'main')",
                params![timeline_id, session_id, Utc::now().to_rfc3339()],
            )
            .unwrap();
        let ctx_id = Uuid::new_v4().to_string();
        store
            .create_replay_context(&ctx_id, session_id, &timeline_id, 0)
            .unwrap();
        ctx_id
    }

    fn fake_session_for_job(store: &Store) -> String {
        use crate::{Session, SessionSource, SessionStatus};
        let session = Session {
            id: Uuid::new_v4().to_string(),
            name: "test-session".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            status: SessionStatus::Recording,
            source: SessionSource::Hooks,
            total_steps: 0,
            total_tokens: 0,
            metadata: serde_json::json!({}),
            thread_id: None,
            thread_ordinal: None,
            client_session_key: None,
        };
        store.create_session(&session).unwrap();
        session.id
    }

    fn fake_job(store: &Store, session_id: &str) -> ReplayJob {
        let ctx_id = fake_replay_context_for_job(store, session_id);
        ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session_id.to_string(),
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Pending,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
            dispatch_token: None,
        }
    }

    #[test]
    fn replay_job_round_trip() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);

        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();
        let fetched = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.state, ReplayJobState::Pending);
        assert!(fetched.runner_id.is_none());
        assert_eq!(fetched.session_id, session_id);
        assert_eq!(fetched.progress_step, 0);
    }

    #[test]
    fn advance_state_sets_correct_timestamp() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();

        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::Dispatched, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Dispatched);
        assert!(after.dispatched_at.is_some());

        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::InProgress, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.started_at.is_some());

        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.completed_at.is_some());
    }

    #[test]
    fn advance_state_refuses_terminal_transitions() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();

        store
            .advance_replay_job_state(&job.id, ReplayJobState::Errored, Some("agent died"), Some("agent"))
            .unwrap();

        let result = store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap();
        assert!(!result, "terminal state must not accept further transitions");

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(after.error_message.as_deref(), Some("agent died"));
        assert_eq!(after.error_stage.as_deref(), Some("agent"));
    }

    #[test]
    fn list_expired_jobs_finds_lease_expired() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);

        let mut job1 = fake_job(&store, &session_id);
        job1.state = ReplayJobState::InProgress;
        job1.lease_expires_at = Some(Utc::now() - chrono::Duration::seconds(60));
        store.create_replay_job(&job1).unwrap();

        let mut job2 = fake_job(&store, &session_id);
        job2.state = ReplayJobState::InProgress;
        job2.lease_expires_at = Some(Utc::now() + chrono::Duration::minutes(5));
        store.create_replay_job(&job2).unwrap();

        let mut job3 = fake_job(&store, &session_id);
        job3.state = ReplayJobState::Dispatched;
        job3.dispatch_deadline_at = Some(Utc::now() - chrono::Duration::seconds(30));
        store.create_replay_job(&job3).unwrap();

        let mut job4 = fake_job(&store, &session_id);
        job4.state = ReplayJobState::Completed;
        job4.lease_expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        store.create_replay_job(&job4).unwrap();

        let expired = store.list_expired_replay_jobs().unwrap();
        let ids: std::collections::HashSet<_> = expired.iter().map(|j| j.id.clone()).collect();
        assert!(ids.contains(&job1.id), "lease-expired in_progress job should be listed");
        assert!(ids.contains(&job3.id), "dispatch-deadline-expired job should be listed");
        assert!(!ids.contains(&job2.id), "future-lease job should NOT be listed");
        assert!(!ids.contains(&job4.id), "terminal job should NOT be listed");
    }

    #[test]
    fn extend_lease_only_works_on_active_states() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &session_id);
        job.state = ReplayJobState::InProgress;
        store.create_replay_job(&job).unwrap();

        let new_lease = Utc::now() + chrono::Duration::minutes(10);
        store.extend_replay_job_lease(&job.id, new_lease).unwrap();
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.lease_expires_at.is_some());

        store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap();
        let lease_after_terminal = store.get_replay_job(&job.id).unwrap().unwrap().lease_expires_at;
        let attempt = Utc::now() + chrono::Duration::minutes(20);
        store.extend_replay_job_lease(&job.id, attempt).unwrap();
        let after_attempt = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(
            after_attempt.lease_expires_at, lease_after_terminal,
            "lease extend on terminal job should be no-op"
        );
    }

    #[test]
    fn append_and_list_events_preserves_order() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();

        for (i, ev_type) in [
            ReplayJobEventType::Started,
            ReplayJobEventType::Progress,
            ReplayJobEventType::Completed,
        ]
        .iter()
        .enumerate()
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
            store
                .append_replay_job_event(&ReplayJobEvent {
                    id: Uuid::new_v4().to_string(),
                    job_id: job.id.clone(),
                    event_type: ev_type.clone(),
                    step_number: Some(i as u32),
                    payload: Some(format!(r#"{{"i":{i}}}"#)),
                    created_at: Utc::now(),
                })
                .unwrap();
        }

        let events = store.list_replay_job_events(&job.id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, ReplayJobEventType::Started);
        assert_eq!(events[1].event_type, ReplayJobEventType::Progress);
        assert_eq!(events[2].event_type, ReplayJobEventType::Completed);
    }

    #[test]
    fn job_state_terminal_check() {
        assert!(!ReplayJobState::Pending.is_terminal());
        assert!(!ReplayJobState::Dispatched.is_terminal());
        assert!(!ReplayJobState::InProgress.is_terminal());
        assert!(ReplayJobState::Completed.is_terminal());
        assert!(ReplayJobState::Errored.is_terminal());
    }

    #[test]
    fn atomic_event_record_rejects_late_event_after_terminal() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();

        store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap();

        let late_event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(99),
            payload: Some(r#"{"step":"99"}"#.to_string()),
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&late_event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "late progress event after terminal must be rejected");

        let events = store.list_replay_job_events(&job.id).unwrap();
        assert!(
            !events.iter().any(|e| e.id == late_event.id),
            "rolled-back transaction left the event row in the DB"
        );

        let final_job = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(final_job.state, ReplayJobState::Completed);
    }

    #[test]
    fn atomic_started_event_transitions_state_and_extends_lease() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &session_id);
        job.state = ReplayJobState::Dispatched;
        store.create_replay_job(&job).unwrap();

        let started_event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Started,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&started_event, None, None, None, 300)
            .unwrap();
        assert!(accepted);

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::InProgress);
        assert!(after.started_at.is_some());
        assert!(after.lease_expires_at.is_some());

        let lease = after.lease_expires_at.unwrap();
        let expected_min = Utc::now() + chrono::Duration::seconds(290);
        let expected_max = Utc::now() + chrono::Duration::seconds(310);
        assert!(
            lease >= expected_min && lease <= expected_max,
            "lease should be ~5min in the future, got {lease}"
        );

        let events = store.list_replay_job_events(&job.id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, ReplayJobEventType::Started);
    }

    #[test]
    fn atomic_progress_event_updates_progress_and_lease_no_state_change() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &session_id);
        job.state = ReplayJobState::InProgress;
        store.create_replay_job(&job).unwrap();

        let progress_event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(7),
            payload: Some(r#"{"step":"7"}"#.to_string()),
            created_at: Utc::now(),
        };
        store
            .record_replay_job_event_atomic(&progress_event, Some(20), None, None, 300)
            .unwrap();

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::InProgress, "progress events don't change state");
        assert_eq!(after.progress_step, 7);
        assert_eq!(after.progress_total, Some(20));
    }

    #[test]
    fn create_active_job_without_replay_context_id_is_rejected() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session_id.clone(),
            replay_context_id: None,
            state: ReplayJobState::Pending,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
            dispatch_token: None,
        };
        let err = store.create_replay_job(&job).unwrap_err();
        assert!(
            err.to_string().contains("replay_context_id is required"),
            "expected replay_context_id required error, got: {err}"
        );
    }

    #[test]
    fn create_terminal_job_with_null_fks_is_allowed_for_import_path() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);

        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session_id.clone(),
            replay_context_id: None,
            state: ReplayJobState::Completed,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: Some(Utc::now()),
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
            dispatch_token: None,
        };
        store.create_replay_job(&job).unwrap();

        let fetched = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.state, ReplayJobState::Completed);
        assert!(fetched.runner_id.is_none());
        assert!(fetched.replay_context_id.is_none());
    }

    #[test]
    fn create_errored_job_with_null_fks_is_allowed() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);

        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session_id.clone(),
            replay_context_id: None,
            state: ReplayJobState::Errored,
            error_message: Some("agent crashed".into()),
            error_stage: Some("agent".into()),
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: Some(Utc::now()),
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
            dispatch_token: None,
        };
        store.create_replay_job(&job).unwrap();
    }

    #[test]
    fn pending_state_rejects_progress_event() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(1),
            payload: Some("{}".into()),
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Progress on Pending must be rejected");
        let events = store.list_replay_job_events(&job.id).unwrap();
        assert!(events.is_empty(), "rejected event must not insert a row");

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Pending);
        assert_eq!(after.progress_step, 0);
    }

    #[test]
    fn pending_state_rejects_completed_event() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Completed,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Completed on Pending must be rejected");
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Pending);
    }

    #[test]
    fn dispatched_state_rejects_progress_event() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &session_id);
        job.state = ReplayJobState::Dispatched;
        store.create_replay_job(&job).unwrap();

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(1),
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Progress on Dispatched must be rejected");
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Dispatched);
    }

    #[test]
    fn in_progress_state_rejects_duplicate_started() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &session_id);
        job.state = ReplayJobState::InProgress;
        job.started_at = Some(Utc::now() - chrono::Duration::seconds(60));
        store.create_replay_job(&job).unwrap();
        let original_started_at = job.started_at;

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Started,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Duplicate Started on InProgress must be rejected");

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.started_at, original_started_at);
    }

    #[test]
    fn dispatched_state_accepts_errored_for_startup_failure() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &session_id);
        job.state = ReplayJobState::Dispatched;
        store.create_replay_job(&job).unwrap();

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Errored,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(
                &event,
                None,
                Some("agent crashed at startup"),
                Some("agent"),
                300,
            )
            .unwrap();
        assert!(accepted, "Errored from Dispatched (startup failure) must be allowed");

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(
            after.error_message.as_deref(),
            Some("agent crashed at startup")
        );
    }

    #[test]
    fn legal_full_lifecycle_accepts_all_events_in_order() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &session_id);
        store.create_replay_job(&job).unwrap();

        store
            .advance_replay_job_state(&job.id, ReplayJobState::Dispatched, None, None)
            .unwrap();

        let started = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Started,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&started, None, None, None, 300)
            .unwrap());

        let prog1 = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(1),
            payload: Some(r#"{"step":1}"#.into()),
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&prog1, Some(10), None, None, 300)
            .unwrap());

        let prog5 = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(5),
            payload: None,
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&prog5, Some(10), None, None, 300)
            .unwrap());

        let completed = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Completed,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&completed, None, None, None, 300)
            .unwrap());

        let final_job = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(final_job.state, ReplayJobState::Completed);
        assert_eq!(final_job.progress_step, 5);
        assert_eq!(final_job.progress_total, Some(10));
        assert_eq!(
            store.list_replay_job_events(&job.id).unwrap().len(),
            4,
            "all 4 events should be persisted"
        );
    }

    #[test]
    fn atomic_errored_event_records_error_fields() {
        let (mut store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &session_id);
        job.state = ReplayJobState::InProgress;
        store.create_replay_job(&job).unwrap();

        let errored = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Errored,
            step_number: None,
            payload: Some(r#"{"error":"agent died"}"#.to_string()),
            created_at: Utc::now(),
        };
        store
            .record_replay_job_event_atomic(
                &errored,
                None,
                Some("agent died at step 5"),
                Some("agent"),
                300,
            )
            .unwrap();

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(after.error_message.as_deref(), Some("agent died at step 5"));
        assert_eq!(after.error_stage.as_deref(), Some("agent"));
        assert!(after.completed_at.is_some());
    }
}
