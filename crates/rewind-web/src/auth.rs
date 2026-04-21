//! Bearer-token auth middleware for the Rewind web API.
//!
//! See `docs/security-audit.md` §CRITICAL-02. Fail-closed on non-loopback binds.
//!
//! Behavior:
//! - `auth_token` resolved at startup from (1) CLI flag, (2) `REWIND_AUTH_TOKEN`,
//!   (3) a file at `~/.rewind/auth_token` auto-generated on first run.
//! - When `AppState::auth_token` is `None`, the middleware is a no-op (loopback
//!   default preserves the current dev UX).
//! - When set, requests to protected routes must send `Authorization: Bearer <token>`.
//!   Comparison is constant-time (`subtle::ConstantTimeEq`).
//! - `/_rewind/health` and static SPA assets are mounted outside this middleware,
//!   so they remain accessible without a token (see `lib.rs::build_router`).

use std::path::{Path, PathBuf};

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rand::RngCore;
use subtle::ConstantTimeEq;

use crate::AppState;

/// How a token was resolved — used only for startup logging.
#[derive(Debug)]
pub enum TokenSource {
    CliFlag,
    EnvVar,
    ExistingFile(PathBuf),
    Generated(PathBuf),
}

/// Resolve the auth token in priority order:
/// 1. Explicit `cli_override` (set by `--auth-token` flag)
/// 2. `REWIND_AUTH_TOKEN` env var (via `std::env::var`)
/// 3. File at `{data_dir}/auth_token` — auto-generated (64 hex chars) if missing
///
/// Returns `Ok((token, source))`. The file, if created, is chmod 0600 on unix.
pub fn resolve_or_generate_token(
    cli_override: Option<String>,
    data_dir: &Path,
) -> anyhow::Result<(String, TokenSource)> {
    resolve_with_env(cli_override, std::env::var("REWIND_AUTH_TOKEN").ok(), data_dir)
}

/// Test-injectable variant: takes the env value as an explicit argument rather
/// than reading `std::env`. Production code should prefer `resolve_or_generate_token`.
pub fn resolve_with_env(
    cli_override: Option<String>,
    env_value: Option<String>,
    data_dir: &Path,
) -> anyhow::Result<(String, TokenSource)> {
    if let Some(tok) = cli_override.filter(|t| !t.is_empty()) {
        return Ok((tok, TokenSource::CliFlag));
    }
    if let Some(tok) = env_value.filter(|t| !t.is_empty()) {
        return Ok((tok, TokenSource::EnvVar));
    }

    let path = data_dir.join("auth_token");
    if path.exists() {
        let tok = std::fs::read_to_string(&path)?.trim().to_string();
        if !tok.is_empty() {
            return Ok((tok, TokenSource::ExistingFile(path)));
        }
        // Empty file — fall through to regenerate.
    }

    // Generate and persist.
    std::fs::create_dir_all(data_dir)?;
    let token = generate_token();
    std::fs::write(&path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    #[cfg(not(unix))]
    {
        tracing::warn!(
            "Auth token file created at {} — rely on OS ACLs for access control (non-unix)",
            path.display()
        );
    }
    Ok((token, TokenSource::Generated(path)))
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Middleware that enforces Bearer-token auth when `AppState::auth_token` is `Some`.
///
/// No-op when the token is `None` (preserving the loopback-unauthenticated default).
pub async fn auth_middleware<B>(
    State(state): State<AppState>,
    req: Request<B>,
    next: Next,
) -> Response
where
    B: axum::body::HttpBody<Data = axum::body::Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let Some(ref expected) = state.auth_token else {
        // Auth disabled — pass through.
        return next.run(transmute_body(req)).await;
    };

    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim());

    let ok = match presented {
        Some(tok) => {
            // Constant-time equality — only compares when lengths match.
            let a = tok.as_bytes();
            let b = expected.as_bytes();
            a.len() == b.len() && bool::from(a.ct_eq(b))
        }
        None => false,
    };

    if ok {
        next.run(transmute_body(req)).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            r#"{"error":"invalid or missing auth token"}"#,
        )
            .into_response()
    }
}

/// `Next` expects `Request<Body>`; the incoming generic `Request<B>` needs to be
/// normalized. We rebuild with `axum::body::Body` to satisfy the middleware
/// signature used by `from_fn_with_state`.
fn transmute_body<B>(req: Request<B>) -> Request<axum::body::Body>
where
    B: axum::body::HttpBody<Data = axum::body::Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let (parts, body) = req.into_parts();
    Request::from_parts(parts, axum::body::Body::new(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_is_64_hex_chars() {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // Two calls should differ with overwhelming probability.
        assert_ne!(t, generate_token());
    }

    #[test]
    fn resolve_prefers_cli_override() {
        let dir = tempfile::tempdir().unwrap();
        let (tok, src) = resolve_with_env(
            Some("from-cli".into()),
            Some("from-env".into()),
            dir.path(),
        )
        .unwrap();
        assert_eq!(tok, "from-cli");
        assert!(matches!(src, TokenSource::CliFlag));
    }

    #[test]
    fn resolve_prefers_env_over_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auth_token"), "from-file").unwrap();
        let (tok, src) =
            resolve_with_env(None, Some("from-env".into()), dir.path()).unwrap();
        assert_eq!(tok, "from-env");
        assert!(matches!(src, TokenSource::EnvVar));
    }

    #[test]
    fn resolve_reads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auth_token"), "stored-token\n").unwrap();
        let (tok, src) = resolve_with_env(None, None, dir.path()).unwrap();
        assert_eq!(tok, "stored-token");
        assert!(matches!(src, TokenSource::ExistingFile(_)));
    }

    #[test]
    fn resolve_generates_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let (tok, src) = resolve_with_env(None, None, dir.path()).unwrap();
        assert_eq!(tok.len(), 64);
        assert!(matches!(src, TokenSource::Generated(_)));
        let on_disk = std::fs::read_to_string(dir.path().join("auth_token")).unwrap();
        assert_eq!(on_disk.trim(), tok);
    }

    #[test]
    fn resolve_treats_empty_cli_and_env_as_unset() {
        let dir = tempfile::tempdir().unwrap();
        let (_tok, src) =
            resolve_with_env(Some("".into()), Some("".into()), dir.path()).unwrap();
        assert!(matches!(src, TokenSource::Generated(_)));
    }

    #[cfg(unix)]
    #[test]
    fn generated_token_file_is_mode_600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (_tok, _src) = resolve_with_env(None, None, dir.path()).unwrap();
        let meta = std::fs::metadata(dir.path().join("auth_token")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
