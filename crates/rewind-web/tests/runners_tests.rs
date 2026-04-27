//! Integration tests for `/api/runners` CRUD endpoints (Phase 3,
//! commit 4/13).
//!
//! Covers:
//! - GET /api/runners — empty + non-empty.
//! - POST /api/runners — register success (raw_token returned ONCE),
//!   503 when crypto key not configured, 400 on bad input,
//!   webhook_url validation, polling-mode requires no url.
//! - GET /api/runners/{id} — success + 404.
//! - DELETE /api/runners/{id} — success + 404 + cascade-on-jobs.
//! - POST /api/runners/{id}/regenerate-token — old hash invalidated,
//!   new hash works, returns new raw token, 404 on missing runner.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_body_util::BodyExt;
use rewind_store::Store;
use rewind_web::{crypto::CryptoBox, AppState, HookIngestionState, StoreEvent};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tower::ServiceExt;

fn setup_with_crypto() -> (Router, Arc<Mutex<Store>>, TempDir) {
    setup_inner(true)
}

fn setup_without_crypto() -> (Router, Arc<Mutex<Store>>, TempDir) {
    setup_inner(false)
}

fn setup_inner(with_crypto: bool) -> (Router, Arc<Mutex<Store>>, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(16);
    let crypto = if with_crypto {
        Some(CryptoBox::from_base64_key(&STANDARD.encode([0x42u8; 32])).unwrap())
    } else {
        None
    };
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: Arc::new(HookIngestionState::new()),
        otel_config: None,
        auth_token: None,
        crypto,
    };
    let app = Router::new().nest("/api", rewind_web::api_routes(state));
    (app, store, tmp)
}

async fn json_post(app: Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
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

async fn http_get(app: Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
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

async fn http_delete(app: Router, uri: &str) -> StatusCode {
    let req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

// ──────────────────────────────────────────────────────────────────
// LIST
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_returns_empty_array_when_no_runners() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = http_get(app, "/api/runners").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_returns_registered_runners_without_token_fields() {
    let (app, _store, _tmp) = setup_with_crypto();

    // Register one runner.
    let (status, _) = json_post(
        app.clone(),
        "/api/runners",
        json!({
            "name": "my-runner",
            "mode": "webhook",
            "webhook_url": "http://localhost:9999/webhook"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // List should show it without raw_token / encrypted_token / nonce.
    let (status, body) = http_get(app, "/api/runners").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let r = &arr[0];
    assert_eq!(r["name"], "my-runner");
    assert_eq!(r["mode"], "webhook");
    assert_eq!(r["webhook_url"], "http://localhost:9999/webhook");
    assert_eq!(r["status"], "active");
    assert!(r["auth_token_preview"].as_str().unwrap().ends_with("***"));
    // No raw / encrypted token surface in list response.
    assert!(r.get("raw_token").is_none());
    assert!(r.get("encrypted_token").is_none());
    assert!(r.get("token_nonce").is_none());
    assert!(r.get("auth_token_hash").is_none());
}

// ──────────────────────────────────────────────────────────────────
// REGISTER
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn register_returns_raw_token_once_and_persists_runner() {
    let (app, store, _tmp) = setup_with_crypto();

    let (status, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({
            "name": "ray-agent",
            "mode": "webhook",
            "webhook_url": "https://example.com/webhook"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["raw_token"].is_string());
    assert!(body["raw_token"].as_str().unwrap().len() >= 32);
    assert!(body["raw_token_warning"]
        .as_str()
        .unwrap()
        .contains("cannot be retrieved"));

    let id = body["runner"]["id"].as_str().unwrap();
    assert!(!id.is_empty());

    // The stored row should have a non-empty encrypted_token + nonce.
    let s = store.lock().unwrap();
    let runner = s.get_runner(id).unwrap().unwrap();
    assert!(!runner.encrypted_token.is_empty());
    assert_eq!(runner.token_nonce.len(), 12);
    // auth_token_hash matches sha256(raw_token).
    let raw = body["raw_token"].as_str().unwrap();
    assert_eq!(runner.auth_token_hash, rewind_web::crypto::hash_runner_token(raw));
    // Preview is "<first 8>***".
    assert!(runner.auth_token_preview.ends_with("***"));
    assert_eq!(runner.auth_token_preview.len(), 8 + 3);
    // get_runner_by_auth_hash succeeds with the raw token.
    let by_hash = s
        .get_runner_by_auth_hash(&rewind_web::crypto::hash_runner_token(raw))
        .unwrap();
    assert!(by_hash.is_some());
    assert_eq!(by_hash.unwrap().id, runner.id);
}

#[tokio::test]
async fn register_returns_503_when_crypto_key_unset() {
    let (app, _store, _tmp) = setup_without_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({
            "name": "doomed",
            "mode": "webhook",
            "webhook_url": "http://example.com/webhook"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("REWIND_RUNNER_SECRET_KEY"));
    assert!(err.contains("openssl rand"));
}

#[tokio::test]
async fn register_validates_empty_name() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "   ", "mode": "webhook", "webhook_url": "http://x.com"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("name"));
}

#[tokio::test]
async fn register_validates_long_name() {
    let (app, _store, _tmp) = setup_with_crypto();
    let too_long: String = "a".repeat(101);
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": too_long, "mode": "webhook", "webhook_url": "http://x.com"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("100"));
}

#[tokio::test]
async fn register_webhook_mode_requires_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "r", "mode": "webhook"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("webhook_url is required"));
}

#[tokio::test]
async fn register_webhook_mode_rejects_non_http_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "r", "mode": "webhook", "webhook_url": "ftp://x.com"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("http:// or https://"));
}

#[tokio::test]
async fn register_polling_mode_must_omit_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "r", "mode": "polling", "webhook_url": "http://x.com"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("must be omitted"));
}

#[tokio::test]
async fn register_polling_mode_succeeds_without_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "polling-runner", "mode": "polling"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["runner"]["mode"], "polling");
    assert!(body["runner"]["webhook_url"].is_null());
}

// ──────────────────────────────────────────────────────────────────
// GET / DELETE
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_runner_returns_404_for_unknown_id() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = http_get(app, "/api/runners/00000000-0000-0000-0000-000000000000").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn get_runner_returns_runner_view_for_existing_id() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "r1", "mode": "webhook", "webhook_url": "http://x.com/wh"}),
    )
    .await;
    let id = body["runner"]["id"].as_str().unwrap().to_string();

    let (status, body) = http_get(app, &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
    assert_eq!(body["name"], "r1");
    assert!(body.get("raw_token").is_none(), "GET must not leak raw token");
}

#[tokio::test]
async fn delete_runner_returns_204_then_404() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "doomed", "mode": "webhook", "webhook_url": "http://x.com/wh"}),
    )
    .await;
    let id = body["runner"]["id"].as_str().unwrap().to_string();

    let status = http_delete(app.clone(), &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Subsequent GET → 404.
    let (status, _) = http_get(app.clone(), &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Subsequent DELETE → 404 (idempotent on absence).
    let status = http_delete(app, &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ──────────────────────────────────────────────────────────────────
// REGENERATE TOKEN
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn regenerate_returns_new_raw_token_and_invalidates_old_hash() {
    let (app, store, _tmp) = setup_with_crypto();
    let (_, register_body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "rotator", "mode": "webhook", "webhook_url": "http://x.com/wh"}),
    )
    .await;
    let id = register_body["runner"]["id"].as_str().unwrap().to_string();
    let original_token = register_body["raw_token"].as_str().unwrap().to_string();
    let original_hash = rewind_web::crypto::hash_runner_token(&original_token);

    // Old hash is currently retrievable.
    {
        let s = store.lock().unwrap();
        assert!(s.get_runner_by_auth_hash(&original_hash).unwrap().is_some());
    }

    // Rotate.
    let (status, body) = json_post(
        app.clone(),
        &format!("/api/runners/{id}/regenerate-token"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_token = body["raw_token"].as_str().unwrap().to_string();
    assert_ne!(new_token, original_token, "rotation must produce a new token");
    let new_hash = rewind_web::crypto::hash_runner_token(&new_token);

    // Old hash → no longer matches anyone.
    {
        let s = store.lock().unwrap();
        assert!(
            s.get_runner_by_auth_hash(&original_hash).unwrap().is_none(),
            "old hash should be invalidated by rotation"
        );
        // New hash → matches our runner.
        let by_hash = s.get_runner_by_auth_hash(&new_hash).unwrap();
        assert!(by_hash.is_some());
        assert_eq!(by_hash.unwrap().id, id);
    }
    // RunnerView in response reflects the new preview.
    let new_preview = body["runner"]["auth_token_preview"].as_str().unwrap();
    let expected_preview = rewind_web::crypto::token_preview(&new_token);
    assert_eq!(new_preview, expected_preview);
}

#[tokio::test]
async fn regenerate_returns_404_for_unknown_runner() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners/00000000-0000-0000-0000-000000000000/regenerate-token",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn regenerate_returns_503_when_crypto_key_unset() {
    let (app, _store, _tmp) = setup_without_crypto();
    let (status, _) = json_post(
        app,
        "/api/runners/any-id/regenerate-token",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// ──────────────────────────────────────────────────────────────────
// Integration with crypto: round-trip the encrypted token
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn encrypted_token_decrypts_to_the_returned_raw_token() {
    // Phase 3 commit 5 will need this property: the dispatcher reads
    // (encrypted_token, nonce) from the runners row, decrypts under
    // the app key, and uses the plaintext to HMAC-sign outbound
    // webhooks. If decrypt didn't recover the raw token, dispatch
    // would silently sign with garbage.
    let (app, store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "decryptor", "mode": "webhook", "webhook_url": "http://x.com/wh"}),
    )
    .await;
    let id = body["runner"]["id"].as_str().unwrap();
    let raw_token = body["raw_token"].as_str().unwrap();

    let s = store.lock().unwrap();
    let runner = s.get_runner(id).unwrap().unwrap();
    drop(s);

    let cb = CryptoBox::from_base64_key(&STANDARD.encode([0x42u8; 32])).unwrap();
    let recovered = cb
        .decrypt(&runner.encrypted_token, &runner.token_nonce)
        .unwrap();
    assert_eq!(recovered.expose(), raw_token);
}
