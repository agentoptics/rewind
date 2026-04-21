//! Integration tests for the auth middleware & fail-closed bind check.
//!
//! See docs/security-audit.md §CRITICAL-02 and plans/squishy-strolling-bachman.md.

use std::net::SocketAddr;

use rewind_store::Store;
use rewind_web::WebServer;
use reqwest::StatusCode;
use tempfile::TempDir;

// ── Fail-closed startup ──────────────────────────────────────

#[tokio::test]
async fn run_refuses_nonloopback_bind_without_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store); // no token

    // 0.0.0.0 is non-loopback per IpAddr::is_loopback
    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        server.run(addr),
    )
    .await
    .expect("server.run should return an error immediately, not block");

    let err = result.expect_err("expected fail-closed error");
    let msg = err.to_string();
    assert!(
        msg.contains("non-loopback") || msg.contains("auth token"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn run_allows_loopback_bind_without_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store);

    // Loopback + no token should NOT immediately error out (backward compat).
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let fut = server.run(addr);

    let result = tokio::time::timeout(std::time::Duration::from_millis(150), fut).await;
    match result {
        Err(_elapsed) => {} // expected: still running
        Ok(Ok(_)) => panic!("server returned Ok unexpectedly"),
        Ok(Err(e)) => panic!("server failed on loopback without token: {e}"),
    }
}

#[tokio::test]
async fn run_allows_nonloopback_with_auth_disabled() {
    // `--no-auth` escape hatch: non-loopback bind with no token AND
    // explicit opt-out should start.
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store).with_auth_disabled(true);

    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let fut = server.run(addr);
    let result = tokio::time::timeout(std::time::Duration::from_millis(150), fut).await;
    match result {
        Err(_elapsed) => {}
        Ok(Ok(_)) => panic!("server returned Ok unexpectedly"),
        Ok(Err(e)) => panic!("--no-auth should bypass fail-closed check: {e}"),
    }
}

#[tokio::test]
async fn run_allows_nonloopback_with_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store).with_auth_token(Some("t".into()));

    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let fut = server.run(addr);
    let result = tokio::time::timeout(std::time::Duration::from_millis(150), fut).await;
    match result {
        Err(_elapsed) => {}
        Ok(Ok(_)) => panic!("server returned Ok unexpectedly"),
        Ok(Err(e)) => panic!("server failed on 0.0.0.0 with token: {e}"),
    }
}

// ── Live-server middleware behavior ─────────────────────────

/// Spawn a server on a loopback ephemeral port. Returns the bound address.
/// The server runs until the test ends (tokio runtime shutdown).
async fn spawn_server_with_token(token: Option<String>) -> SocketAddr {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    // Keep the tempdir alive for the server's lifetime by leaking it.
    // Test processes are short-lived; the OS reclaims on exit.
    let _ = tmp.keep();

    // Pick a port by binding, reading local_addr, then dropping.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let server = WebServer::new_standalone(store).with_auth_token(token);
    tokio::spawn(async move {
        let _ = server.run(addr).await;
    });

    // Wait briefly for the server to bind.
    for _ in 0..40 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            // Give axum a beat to register routes after TCP accept.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            return addr;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("server did not start on {addr}");
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

async fn http_get(
    addr: SocketAddr,
    path: &str,
    auth: Option<&str>,
) -> (StatusCode, String) {
    let url = format!("http://{addr}{path}");
    let mut req = client().get(&url);
    if let Some(tok) = auth {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn health_endpoint_bypasses_auth() {
    let addr = spawn_server_with_token(Some("secret".into())).await;
    let (status, body) = http_get(addr, "/_rewind/health", None).await;
    assert_eq!(status, StatusCode::OK, "health must be accessible without auth");
    assert!(body.contains("\"status\":\"ok\""), "body: {body}");
}

#[tokio::test]
async fn protected_route_requires_token() {
    let addr = spawn_server_with_token(Some("secret".into())).await;
    let (status, body) = http_get(addr, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("invalid or missing auth token"), "body: {body}");
}

#[tokio::test]
async fn protected_route_accepts_correct_token() {
    let addr = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/api/sessions", Some("secret")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn protected_route_rejects_wrong_token() {
    let addr = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/api/sessions", Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn middleware_is_noop_when_token_is_none() {
    let addr = spawn_server_with_token(None).await;
    let (status, _) = http_get(addr, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "no token configured → routes open");
}

#[tokio::test]
async fn eval_route_requires_token() {
    let addr = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/api/eval/datasets", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn hook_route_requires_token() {
    let addr = spawn_server_with_token(Some("secret".into())).await;
    // GET on the POST-only /api/hooks/event: without auth should be 401,
    // with auth should be 405 (method not allowed). We test the unauth case.
    let (status, _) = http_get(addr, "/api/hooks/event", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn otlp_route_requires_token() {
    let addr = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/v1/traces", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
