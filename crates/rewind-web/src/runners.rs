//! Replay-job HTTP endpoints.
//!
//! ## Routes (mounted at `/api`)
//!
//! | method | path                                   | purpose                      |
//! |--------|----------------------------------------|------------------------------|
//! | POST   | `/api/sessions/{sid}/replay-jobs`      | create + dispatch replay job |
//! | GET    | `/api/sessions/{sid}/replay-jobs`      | list jobs for a session      |
//! | GET    | `/api/replay-jobs/{id}`                | get one job                  |
//! | POST   | `/api/replay-jobs/{id}/events`         | runner posts progress events |

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use chrono::Utc;
use rewind_replay::ReplayEngine;
use rewind_store::{ReplayJob, ReplayJobEvent, ReplayJobEventType, ReplayJobState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppState, StoreEvent};

/// Replay context TTL (seconds). Mirrors the constant in
/// `WebServer::run`'s background cleanup task.
const REPLAY_CONTEXT_TTL_SECS: i64 = 3600;

/// Lease duration for replay jobs (5 minutes, extended on heartbeat).
pub const INITIAL_LEASE_SECS: i64 = 300;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/sessions/{sid}/replay-jobs",
            post(create_replay_job).get(list_replay_jobs_for_session),
        )
        .route("/replay-jobs/{id}", get(get_replay_job))
}

/// Runner-callback route for posting replay job events.
/// Mounted OUTSIDE bearer auth middleware — no authentication required
/// (the runner is a co-located sidecar or trusted process).
pub fn runner_callback_routes() -> Router<AppState> {
    Router::new().route(
        "/api/replay-jobs/{id}/events",
        post(post_replay_job_event),
    )
}

// ── Error helpers ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

fn bad_request<E: ToString>(e: E) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
            error: e.to_string(),
        }),
    )
}

fn internal<E: ToString>(e: E) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            error: e.to_string(),
        }),
    )
}

fn not_found(what: &str) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: format!("{what} not found"),
        }),
    )
}

fn conflict(msg: String) -> (StatusCode, Json<ErrorBody>) {
    (StatusCode::CONFLICT, Json(ErrorBody { error: msg }))
}

// ── Create replay job ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateReplayJobShapeA {
    pub source_timeline_id: String,
    pub at_step: u32,
    #[serde(default)]
    pub strict_match: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateReplayJobShapeB {
    pub replay_context_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum CreateReplayJobRequest {
    CreateAndDispatch(CreateReplayJobShapeA),
    ReuseContext(CreateReplayJobShapeB),
}

#[derive(Debug, Serialize)]
pub struct CreateReplayJobResponse {
    pub job_id: String,
    pub replay_context_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_timeline_id: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_deadline_at: Option<String>,
}

/// `POST /api/sessions/{sid}/replay-jobs`
///
/// Creates a replay job and dispatches it to `REWIND_REPLAY_WEBHOOK_URL`
/// with a plain unsigned POST.
async fn create_replay_job(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<CreateReplayJobRequest>,
) -> Result<(StatusCode, Json<CreateReplayJobResponse>), (StatusCode, Json<ErrorBody>)> {
    let webhook_url = state.replay_webhook_url.clone().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "REWIND_REPLAY_WEBHOOK_URL is not configured; replay dispatch unavailable."
                    .to_string(),
            }),
        )
    })?;

    let (job, fork_timeline_id, at_step) = {
        let store = state
            .store
            .lock()
            .map_err(|e| internal(format!("store lock: {e}")))?;
        let session = store
            .get_session(&sid)
            .map_err(internal)?
            .ok_or_else(|| not_found("session"))?;

        let (replay_context_id, fork_timeline_id, at_step) = match req {
            CreateReplayJobRequest::CreateAndDispatch(a) => {
                let timelines = store.get_timelines(&session.id).map_err(internal)?;
                if !timelines.iter().any(|t| t.id == a.source_timeline_id) {
                    return Err(bad_request(format!(
                        "source_timeline_id {} not found in session {}",
                        a.source_timeline_id, session.id
                    )));
                }
                let engine = ReplayEngine::new(&store);
                let fork = engine
                    .fork(
                        &session.id,
                        &a.source_timeline_id,
                        a.at_step,
                        &format!("replay-{}", &Uuid::new_v4().to_string()[..8]),
                    )
                    .map_err(|e| bad_request(format!("fork failed: {e}")))?;
                let ctx_id = Uuid::new_v4().to_string();
                store
                    .create_replay_context(&ctx_id, &session.id, &fork.id, 0)
                    .map_err(internal)?;
                if a.strict_match {
                    store
                        .set_replay_context_strict_match(&ctx_id, true)
                        .map_err(internal)?;
                }
                (ctx_id, Some(fork.id), a.at_step)
            }
            CreateReplayJobRequest::ReuseContext(b) => {
                let ctx = store
                    .get_replay_context(&b.replay_context_id)
                    .map_err(internal)?
                    .ok_or_else(|| not_found("replay_context"))?;
                if ctx.session_id != session.id {
                    return Err(bad_request(format!(
                        "replay_context {} belongs to session {}, not {}",
                        b.replay_context_id, ctx.session_id, session.id
                    )));
                }
                if ctx.current_step != ctx.from_step {
                    return Err(conflict(format!(
                        "replay_context {} cursor already advanced to step {} \
                         (started at {}); a fresh context is required",
                        b.replay_context_id, ctx.current_step, ctx.from_step
                    )));
                }
                let age = chrono::Utc::now()
                    .signed_duration_since(ctx.last_accessed_at)
                    .num_seconds();
                if age > REPLAY_CONTEXT_TTL_SECS {
                    return Err(conflict(format!(
                        "replay_context {} is {}s old (TTL {}s); create a fresh context",
                        b.replay_context_id, age, REPLAY_CONTEXT_TTL_SECS
                    )));
                }
                let in_flight = store
                    .count_in_flight_jobs_for_replay_context(&b.replay_context_id)
                    .map_err(internal)?;
                if in_flight > 0 {
                    return Err(conflict(format!(
                        "replay_context {} already has {in_flight} in-flight job(s)",
                        b.replay_context_id
                    )));
                }
                let at_step = store
                    .get_timelines(&session.id)
                    .map_err(internal)?
                    .iter()
                    .find(|t| t.id == ctx.timeline_id)
                    .and_then(|t| t.fork_at_step)
                    .unwrap_or(1);
                (b.replay_context_id, Some(ctx.timeline_id), at_step)
            }
        };

        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session.id.clone(),
            replay_context_id: Some(replay_context_id),
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
        };
        store.create_replay_job(&job).map_err(internal)?;
        store
            .advance_replay_job_state(&job.id, ReplayJobState::Dispatched, None, None)
            .map_err(internal)?;
        (job, fork_timeline_id, at_step)
    };

    // Dispatch via plain POST to the configured webhook URL.
    let job_clone = job.clone();
    let store_arc = state.store.clone();
    let event_tx = state.event_tx.clone();
    let timeline_for_payload = fork_timeline_id.clone().unwrap_or_default();
    let base_url = state.base_url.clone();
    tokio::spawn(async move {
        let payload = serde_json::json!({
            "job_id": job_clone.id,
            "session_id": job_clone.session_id,
            "replay_context_id": job_clone.replay_context_id,
            "replay_context_timeline_id": timeline_for_payload,
            "at_step": at_step,
            "base_url": base_url,
        });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let result = client
            .post(&webhook_url)
            .header("Content-Type", "application/json")
            .header("X-Rewind-Job-Id", &job_clone.id)
            .json(&payload)
            .send()
            .await;
        let outcome_err = match result {
            Ok(resp) if resp.status().is_success() || resp.status() == reqwest::StatusCode::ACCEPTED => None,
            Ok(resp) => Some(format!("webhook returned {}", resp.status())),
            Err(e) => Some(format!("webhook dispatch failed: {e}")),
        };
        if let Some(err_msg) = &outcome_err {
            if let Ok(store) = store_arc.lock() {
                let _ = store.advance_replay_job_state(
                    &job_clone.id,
                    ReplayJobState::Errored,
                    Some(err_msg),
                    Some("dispatch"),
                );
            }
        } else if let Ok(store) = store_arc.lock() {
            let _ = store.set_dispatch_deadline_and_lease(
                &job_clone.id,
                Utc::now() + chrono::Duration::seconds(10),
                Utc::now() + chrono::Duration::seconds(INITIAL_LEASE_SECS),
            );
        }
        if let Ok(store) = store_arc.lock() {
            if let Ok(Some(after)) = store.get_replay_job(&job_clone.id) {
                let _ = event_tx.send(StoreEvent::ReplayJobUpdated {
                    job_id: after.id.clone(),
                    session_id: after.session_id.clone(),
                    state: after.state.as_str().to_string(),
                    progress_step: Some(after.progress_step),
                    progress_total: after.progress_total,
                    error_message: after.error_message.clone(),
                    error_stage: after.error_stage.clone(),
                });
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateReplayJobResponse {
            job_id: job.id.clone(),
            replay_context_id: job.replay_context_id.clone().unwrap_or_default(),
            fork_timeline_id,
            state: "dispatched".to_string(),
            dispatch_deadline_at: job.dispatch_deadline_at.map(|t| t.to_rfc3339()),
        }),
    ))
}

// ── ReplayJobView ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ReplayJobView {
    pub id: String,
    pub runner_id: Option<String>,
    pub session_id: String,
    pub replay_context_id: Option<String>,
    pub state: String,
    pub error_message: Option<String>,
    pub error_stage: Option<String>,
    pub progress_step: u32,
    pub progress_total: Option<u32>,
    pub created_at: String,
    pub dispatched_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub dispatch_deadline_at: Option<String>,
    pub lease_expires_at: Option<String>,
}

impl From<ReplayJob> for ReplayJobView {
    fn from(j: ReplayJob) -> Self {
        Self {
            id: j.id,
            runner_id: j.runner_id,
            session_id: j.session_id,
            replay_context_id: j.replay_context_id,
            state: j.state.as_str().to_string(),
            error_message: j.error_message,
            error_stage: j.error_stage,
            progress_step: j.progress_step,
            progress_total: j.progress_total,
            created_at: j.created_at.to_rfc3339(),
            dispatched_at: j.dispatched_at.map(|t| t.to_rfc3339()),
            started_at: j.started_at.map(|t| t.to_rfc3339()),
            completed_at: j.completed_at.map(|t| t.to_rfc3339()),
            dispatch_deadline_at: j.dispatch_deadline_at.map(|t| t.to_rfc3339()),
            lease_expires_at: j.lease_expires_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// `GET /api/sessions/{sid}/replay-jobs`
async fn list_replay_jobs_for_session(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<Vec<ReplayJobView>>, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let jobs = store
        .list_replay_jobs_by_session(&sid)
        .map_err(internal)?;
    Ok(Json(jobs.into_iter().map(ReplayJobView::from).collect()))
}

/// `GET /api/replay-jobs/{id}`
async fn get_replay_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ReplayJobView>, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let job = store
        .get_replay_job(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("replay_job"))?;
    Ok(Json(job.into()))
}

// ── Runner callback: POST /api/replay-jobs/{id}/events ───────────

#[derive(Debug, Deserialize)]
pub struct PostReplayJobEventRequest {
    pub event_type: ReplayJobEventType,
    pub step_number: Option<u32>,
    pub progress_total: Option<u32>,
    pub payload: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub error_stage: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PostReplayJobEventResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub state: String,
}

/// `POST /api/replay-jobs/{id}/events`
///
/// No authentication required — the runner is a co-located sidecar
/// or trusted process communicating over localhost/cluster-internal network.
async fn post_replay_job_event(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(req): Json<PostReplayJobEventRequest>,
) -> Result<(StatusCode, Json<PostReplayJobEventResponse>), (StatusCode, Json<ErrorBody>)> {
    let mut store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;

    let job = store
        .get_replay_job(&job_id)
        .map_err(internal)?
        .ok_or_else(|| not_found("replay_job"))?;

    let event = ReplayJobEvent {
        id: Uuid::new_v4().to_string(),
        job_id: job.id.clone(),
        event_type: req.event_type,
        step_number: req.step_number,
        payload: req
            .payload
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default()),
        created_at: Utc::now(),
    };
    let accepted = store
        .record_replay_job_event_atomic(
            &event,
            req.progress_total,
            req.error_message.as_deref(),
            req.error_stage.as_deref(),
            INITIAL_LEASE_SECS,
        )
        .map_err(internal)?;

    let after = store
        .get_replay_job(&job.id)
        .map_err(internal)?
        .unwrap_or(job);

    let state_str = after.state.as_str().to_string();
    let reason = if accepted {
        None
    } else {
        Some("event rejected by state machine (terminal or illegal transition)".to_string())
    };

    if accepted {
        let _ = state.event_tx.send(StoreEvent::ReplayJobUpdated {
            job_id: after.id.clone(),
            session_id: after.session_id.clone(),
            state: state_str.clone(),
            progress_step: Some(after.progress_step),
            progress_total: after.progress_total,
            error_message: after.error_message.clone(),
            error_stage: after.error_stage.clone(),
        });
    }

    let status = if accepted {
        StatusCode::ACCEPTED
    } else {
        StatusCode::CONFLICT
    };
    Ok((
        status,
        Json(PostReplayJobEventResponse {
            accepted,
            reason,
            state: state_str,
        }),
    ))
}
