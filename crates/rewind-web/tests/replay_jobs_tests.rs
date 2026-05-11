//! Integration tests for replay-job dispatch + event-ingestion HTTP endpoints.
//!
//! Post runner-registry removal: dispatch goes to a configured webhook URL
//! (plain POST, no HMAC), events are accepted without runner-auth headers.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use chrono::Utc;
use http_body_util::BodyExt;
use rewind_store::{ReplayJob, ReplayJobState, Session, Store, Timeline};
use rewind_web::{runners::runner_callback_routes, AppState, HookIngestionState, StoreEvent};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

fn setup_with_webhook(webhook_url: &str) -> (Router, Router, Arc<Mutex<Store>>, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(64);
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: Arc::new(HookIngestionState::new()),
        otel_config: None,
        auth_token: None,
        replay_webhook_url: Some(webhook_url.to_string()),
        base_url: "http://127.0.0.1:4800".to_string(),
    };
    let api = Router::new().nest("/api", rewind_web::api_routes(state.clone()));
    let callbacks = runner_callback_routes().with_state(state);
    (api, callbacks, store, tmp)
}

fn setup_no_webhook() -> (Router, Router, Arc<Mutex<Store>>, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(64);
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: Arc::new(HookIngestionState::new()),
        otel_config: None,
        auth_token: None,
        replay_webhook_url: None,
        base_url: "http://127.0.0.1:4800".to_string(),
    };
    let api = Router::new().nest("/api", rewind_web::api_routes(state.clone()));
    let callbacks = runner_callback_routes().with_state(state);
    (api, callbacks, store, tmp)
}

async fn json_post(app: Router, uri: &str, body: Value) -> (StatusCode, Value) {
    json_post_with_headers(app, uri, body, vec![]).await
}

async fn json_post_with_headers(
    app: Router,
    uri: &str,
    body: Value,
    extra_headers: Vec<(&str, &str)>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in extra_headers {
        builder = builder.header(k, v);
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

/// Spawn a stub HTTP server that always replies 202. Returns the URL
/// and a receiver for captured (headers, body) pairs.
async fn spawn_webhook_stub() -> (
    String,
    tokio::sync::mpsc::Receiver<(axum::http::HeaderMap, axum::body::Bytes)>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let app = axum::Router::new().route(
        "/wh",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send((headers, body)).await;
                    StatusCode::ACCEPTED
                }
            },
        ),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/wh"), rx)
}

async fn spawn_webhook_stub_500() -> String {
    let app = axum::Router::new().route(
        "/wh",
        axum::routing::post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/wh")
}

fn seed_session_and_context(store: &Arc<Mutex<Store>>) -> (String, String, String) {
    let s = store.lock().unwrap();
    let session = Session::new("dispatch-test-session");
    let session_id = session.id.clone();
    let timeline = Timeline::new_root(&session_id);
    s.create_session(&session).unwrap();
    s.create_timeline(&timeline).unwrap();
    let ctx_id = Uuid::new_v4().to_string();
    s.create_replay_context(&ctx_id, &session_id, &timeline.id, 0)
        .unwrap();
    (session_id, timeline.id, ctx_id)
}

fn seed_session_with_n_steps(store: &Arc<Mutex<Store>>, n: u32) -> (String, String) {
    use rewind_store::{SessionSource, SessionStatus, Step, StepStatus};
    let s = store.lock().unwrap();
    let session = Session {
        id: Uuid::new_v4().to_string(),
        name: "replay-test".into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        status: SessionStatus::Recording,
        source: SessionSource::Hooks,
        total_steps: 0,
        total_tokens: 0,
        metadata: json!({}),
        thread_id: None,
        thread_ordinal: None,
        client_session_key: None,
    };
    let timeline = Timeline::new_root(&session.id);
    s.create_session(&session).unwrap();
    s.create_timeline(&timeline).unwrap();
    for i in 1..=n {
        let mut step = Step::new_llm_call(&timeline.id, &session.id, i, "stub-model");
        step.status = StepStatus::Success;
        step.duration_ms = 10;
        s.create_step(&step).unwrap();
    }
    s.update_session_stats(&session.id, n, 0).unwrap();
    (session.id, timeline.id)
}

// ── Dispatch (shape B: reuse-context) ────────────────────────────

#[tokio::test]
async fn create_replay_job_dispatches_to_webhook_and_transitions_to_dispatched() {
    let (webhook_url, mut rx) = spawn_webhook_stub().await;
    let (api, _callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let (status, body) = json_post(
        api.clone(),
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body:?}");
    let job_id = body["job_id"].as_str().unwrap().to_string();
    assert_eq!(body["state"], "dispatched");
    assert_eq!(body["replay_context_id"], ctx_id);

    let (_headers, body_bytes) =
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("webhook must be called within 3s")
            .expect("stub channel closed");
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(payload["job_id"], job_id);
    assert!(payload["session_id"].is_string());
    assert!(payload["base_url"].is_string());
}

#[tokio::test]
async fn create_replay_job_with_webhook_500_transitions_to_errored() {
    let webhook_url = spawn_webhook_stub_500().await;
    let (api, _callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let (status, body) = json_post(
        api.clone(),
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job_id = body["job_id"].as_str().unwrap().to_string();

    for _ in 0..30 {
        let snapshot = {
            let s = store.lock().unwrap();
            s.get_replay_job(&job_id).unwrap().unwrap()
        };
        if matches!(snapshot.state, ReplayJobState::Errored) {
            assert_eq!(snapshot.error_stage.as_deref(), Some("dispatch"));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job never reached Errored state");
}

#[tokio::test]
async fn create_replay_job_returns_503_when_no_webhook_configured() {
    let (api, _callbacks, _store, _tmp) = setup_no_webhook();
    let (status, body) = json_post(
        api,
        "/api/sessions/00000000-0000-0000-0000-000000000000/replay-jobs",
        json!({"replay_context_id": "y"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("REWIND_REPLAY_WEBHOOK_URL"));
}

#[tokio::test]
async fn create_replay_job_rejects_context_in_use() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (api, _callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session_id.clone(),
            replay_context_id: Some(ctx_id.clone()),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
            dispatch_token: None,
        };
        s.create_replay_job(&job).unwrap();
    }

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("in-flight"));
}

#[tokio::test]
async fn create_replay_job_rejects_unknown_session() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (api, _callbacks, _store, _tmp) = setup_with_webhook(&webhook_url);
    let (status, body) = json_post(
        api,
        "/api/sessions/00000000-0000-0000-0000-000000000000/replay-jobs",
        json!({"replay_context_id": "y"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("session"));
}

// ── Dispatch payload carries at_step ─────────────────────────────

#[tokio::test]
async fn dispatch_payload_carries_at_step() {
    let (webhook_url, mut rx) = spawn_webhook_stub().await;
    let (api, _callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, root_timeline_id) = seed_session_with_n_steps(&store, 4);

    let (status, _body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({
            "source_timeline_id": root_timeline_id,
            "at_step": 4,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (_headers, body_bytes) =
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("webhook must be called within 3s")
            .expect("stub channel closed");
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(payload["at_step"].as_u64(), Some(4));
    assert!(payload["job_id"].is_string());
    assert!(payload["session_id"].is_string());
    assert!(payload["replay_context_id"].is_string());
    assert!(payload["replay_context_timeline_id"].is_string());
    assert!(payload["source_timeline_id"].is_string());
    assert!(payload["base_url"].is_string());
}

// ── Event endpoint (no auth required) ────────────────────────────

#[tokio::test]
async fn event_endpoint_started_transitions_dispatched_to_in_progress() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (_api, callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let token = Uuid::new_v4().to_string();
    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
            dispatch_token: Some(token.clone()),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, body) = json_post_with_headers(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "started"}),
        vec![("X-Rewind-Dispatch-Token", &token)],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["accepted"], true);
    assert_eq!(body["state"], "in_progress");

    let s = store.lock().unwrap();
    let after = s.get_replay_job(&job_id).unwrap().unwrap();
    assert_eq!(after.state, ReplayJobState::InProgress);
    assert!(after.started_at.is_some());
}

#[tokio::test]
async fn event_endpoint_progress_updates_step() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (_api, callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let token = Uuid::new_v4().to_string();
    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::InProgress,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: Some(Utc::now()),
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
            dispatch_token: Some(token.clone()),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, _body) = json_post_with_headers(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "progress", "step_number": 7, "progress_total": 20}),
        vec![("X-Rewind-Dispatch-Token", &token)],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let s = store.lock().unwrap();
    let after = s.get_replay_job(&job_id).unwrap().unwrap();
    assert_eq!(after.state, ReplayJobState::InProgress);
    assert_eq!(after.progress_step, 7);
    assert_eq!(after.progress_total, Some(20));
}

#[tokio::test]
async fn event_endpoint_completed_transitions_to_completed() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (_api, callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let token = Uuid::new_v4().to_string();
    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::InProgress,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: Some(Utc::now()),
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 5,
            progress_total: Some(5),
            dispatch_token: Some(token.clone()),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, _body) = json_post_with_headers(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "completed"}),
        vec![("X-Rewind-Dispatch-Token", &token)],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let s = store.lock().unwrap();
    let after = s.get_replay_job(&job_id).unwrap().unwrap();
    assert_eq!(after.state, ReplayJobState::Completed);
}

#[tokio::test]
async fn event_endpoint_returns_409_on_illegal_transition() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (_api, callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let token = Uuid::new_v4().to_string();
    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id,
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
            dispatch_token: Some(token.clone()),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, body) = json_post_with_headers(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "progress", "step_number": 1}),
        vec![("X-Rewind-Dispatch-Token", &token)],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["accepted"], false);
    assert!(body["reason"].as_str().unwrap().contains("state machine"));
}

// ── Dispatch token enforcement ───────────────────────────────────

#[tokio::test]
async fn event_endpoint_rejects_missing_dispatch_token() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (_api, callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
            dispatch_token: Some("secret-token-123".to_string()),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    // No token header — should be rejected
    let (status, body) = json_post(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "started"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("Dispatch-Token"));
}

// ── Regression: shape-A dispatch sends source_timeline_id (not fork)
//    for reading, replay_context_timeline_id = fork for writing ───────

#[tokio::test]
async fn dispatch_payload_separates_source_and_fork_timeline_ids() {
    let (webhook_url, mut rx) = spawn_webhook_stub().await;
    let (api, _callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, root_timeline_id) = seed_session_with_n_steps(&store, 3);

    let (status, resp) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({
            "source_timeline_id": root_timeline_id,
            "at_step": 2,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {resp:?}");

    // Response fork_timeline_id must be a NEW timeline (the replay fork),
    // not the source timeline we passed in.
    let response_fork_id = resp["fork_timeline_id"]
        .as_str()
        .expect("response must include fork_timeline_id");
    assert_ne!(
        response_fork_id, root_timeline_id,
        "response fork_timeline_id must be the new fork, not the source"
    );

    // Capture the webhook payload.
    let (_headers, body_bytes) =
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("webhook must be called within 3s")
            .expect("stub channel closed");
    let payload: Value = serde_json::from_slice(&body_bytes).unwrap();

    // replay_context_timeline_id (write target) = the new fork
    let payload_write_tl = payload["replay_context_timeline_id"]
        .as_str()
        .expect("payload must include replay_context_timeline_id");
    assert_eq!(
        payload_write_tl, response_fork_id,
        "replay_context_timeline_id in payload must match the fork returned to dashboard"
    );

    // source_timeline_id (read target) = the original source timeline
    let payload_read_tl = payload["source_timeline_id"]
        .as_str()
        .expect("payload must include source_timeline_id");
    assert_eq!(
        payload_read_tl, root_timeline_id,
        "source_timeline_id in payload must be the timeline the user edited"
    );

    // The two must differ (fork != source).
    assert_ne!(
        payload_write_tl, payload_read_tl,
        "write timeline (fork) and read timeline (source) must differ for shape A"
    );
}

#[tokio::test]
async fn event_endpoint_rejects_wrong_dispatch_token() {
    let (webhook_url, _rx) = spawn_webhook_stub().await;
    let (_api, callbacks, store, _tmp) = setup_with_webhook(&webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
            dispatch_token: Some("correct-token".to_string()),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    // Wrong token — should be rejected
    let (status, body) = json_post_with_headers(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "started"}),
        vec![("X-Rewind-Dispatch-Token", "wrong-token")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("Dispatch-Token"));
}
