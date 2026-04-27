//! AES-256-GCM "CryptoBox" for at-rest encryption of runner auth tokens
//! (Phase 3, commit 4/13).
//!
//! ## Threat model
//!
//! Runner auth tokens are shared secrets used to HMAC-sign outbound
//! webhooks (Rewind → runner). The server must be able to recover the
//! raw token at dispatch time, but a hash-only model can't do that
//! (one-way). So the raw token is encrypted at rest under a server-
//! managed app key (`REWIND_RUNNER_SECRET_KEY` env var, base64-encoded
//! 32 bytes) using AES-256-GCM, with a fresh 12-byte nonce per row.
//!
//! AEAD (auth tag) detects tampering: a flipped ciphertext bit yields
//! `Err`, never silent corruption. Confidentiality + integrity in one
//! primitive.
//!
//! ## Bootstrap
//!
//! - Operator generates a key once: `openssl rand -base64 32`.
//! - Sets `REWIND_RUNNER_SECRET_KEY=<base64-32-bytes>` in env.
//! - Server reads it at startup via [`CryptoBox::from_env`].
//! - If unset, runner endpoints return `503 Service Unavailable` with
//!   a clear bootstrap error (handled in `runners.rs`).
//!
//! Key rotation is **out of scope for v1**. Rotating the app key
//! requires re-encrypting every runner row; v3.1 will add a
//! `key_version` column for rolling rotation.
//!
//! ## API surface
//!
//! - [`CryptoBox::from_env`] — read `REWIND_RUNNER_SECRET_KEY`,
//!   returns `Ok(None)` if unset (caller maps to `503`).
//! - [`CryptoBox::from_base64_key`] — parse a key (used in tests +
//!   from_env internally).
//! - [`CryptoBox::fresh_nonce`] — allocate a new random 12-byte nonce.
//! - [`CryptoBox::encrypt`] — encrypt a raw secret to ciphertext.
//! - [`CryptoBox::decrypt`] — recover the raw secret as a
//!   [`SensitiveString`] so accidental log/Debug never leaks it.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::{rngs::OsRng, TryRngCore};
use rewind_store::SensitiveString;

/// AES-256-GCM nonce length (12 bytes per RFC 5116 recommendation).
pub const NONCE_LEN: usize = 12;

/// AES-256 key length (32 bytes).
pub const KEY_LEN: usize = 32;

/// Environment variable holding the base64-encoded 32-byte app key.
pub const KEY_ENV_VAR: &str = "REWIND_RUNNER_SECRET_KEY";

/// AES-256-GCM cipher with a fixed app key.
///
/// Cloning is cheap (the underlying `Aes256Gcm` is `Clone` and holds
/// only the expanded key schedule). Stored in `AppState` and shared
/// across handler tasks.
///
/// `Debug` is manually implemented to redact the key — printing
/// `cipher` would expose the expanded key schedule, which is roughly
/// "the key" for an attacker. We render it as `CryptoBox(<redacted>)`.
#[derive(Clone)]
pub struct CryptoBox {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for CryptoBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CryptoBox(<redacted>)")
    }
}

impl CryptoBox {
    /// Build a CryptoBox from a base64-encoded key. Caller is
    /// responsible for not logging the input.
    ///
    /// Errors if the base64 decode fails or the decoded length is not
    /// exactly 32 bytes (a 31- or 33-byte key would silently work in
    /// unsafe AES bindings; we refuse).
    pub fn from_base64_key(b64: &str) -> Result<Self> {
        let key_bytes = STANDARD
            .decode(b64.trim())
            .with_context(|| format!("{KEY_ENV_VAR}: not valid base64"))?;
        if key_bytes.len() != KEY_LEN {
            bail!(
                "{KEY_ENV_VAR}: expected {KEY_LEN} bytes after base64 decode, got {}",
                key_bytes.len()
            );
        }
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);
        Ok(Self { cipher })
    }

    /// Read `REWIND_RUNNER_SECRET_KEY` from env. Returns `Ok(None)`
    /// if the variable is unset (caller maps to `503`); returns
    /// `Err(_)` if it's set but malformed (operator misconfig — fail
    /// loud at startup).
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var(KEY_ENV_VAR) {
            Ok(b64) if !b64.is_empty() => Self::from_base64_key(&b64).map(Some),
            _ => Ok(None),
        }
    }

    /// Allocate a fresh 12-byte nonce from `OsRng`.
    ///
    /// Each encryption MUST use a unique nonce under the same key.
    /// Nonce reuse breaks AES-GCM confidentiality (key recovery via
    /// XOR of two ciphertexts at the same nonce). 96-bit nonces from
    /// `OsRng` collide with negligible probability for our use case
    /// (the practical bound is ~2^32 messages per key, well above
    /// the runner-registration volume).
    pub fn fresh_nonce() -> [u8; NONCE_LEN] {
        let mut buf = [0u8; NONCE_LEN];
        OsRng.try_fill_bytes(&mut buf).expect("OsRng must succeed");
        buf
    }

    /// Encrypt a plaintext under the supplied nonce. Returns the
    /// ciphertext bytes (auth tag appended by AEAD).
    ///
    /// `nonce` MUST be 12 bytes. `nonce` MUST be unique per
    /// (key, ciphertext) pair — caller's responsibility (we re-emit
    /// a fresh nonce per row in the runners table).
    pub fn encrypt(&self, plaintext: &[u8], nonce: &[u8]) -> Result<Vec<u8>> {
        if nonce.len() != NONCE_LEN {
            bail!("nonce must be {NONCE_LEN} bytes, got {}", nonce.len());
        }
        let nonce = Nonce::from_slice(nonce);
        self.cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("AES-GCM encrypt failed: {e}"))
    }

    /// Decrypt ciphertext to a `SensitiveString`. Returns `Err` on
    /// AEAD verification failure (tampered ciphertext, wrong key,
    /// wrong nonce) — `SensitiveString` ensures the recovered raw
    /// secret never accidentally appears in logs.
    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8]) -> Result<SensitiveString> {
        if nonce.len() != NONCE_LEN {
            bail!("nonce must be {NONCE_LEN} bytes, got {}", nonce.len());
        }
        let nonce = Nonce::from_slice(nonce);
        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("AES-GCM decrypt failed (tampered or wrong key/nonce): {e}"))?;
        let s = String::from_utf8(plaintext)
            .map_err(|e| anyhow!("decrypted plaintext is not valid UTF-8: {e}"))?;
        Ok(SensitiveString::new(s))
    }
}

/// Generate a fresh 32-byte random raw token, base64-url encoded
/// (URL-safe, no padding). Used at runner registration / token
/// regeneration. Returns the raw token as a `SensitiveString` so
/// debug output never accidentally leaks it; the caller serializes
/// the inner string into the one-time API response.
pub fn generate_runner_token() -> SensitiveString {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut buf = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut buf)
        .expect("OsRng must succeed");
    SensitiveString::new(URL_SAFE_NO_PAD.encode(buf))
}

/// Compute the SHA-256 hex digest of a raw runner token. Used for
/// the indexed inbound-auth lookup (`runners.auth_token_hash`).
pub fn hash_runner_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    format!("{:x}", h.finalize())
}

/// Build the public preview displayed in the dashboard alongside
/// the registered runner. Format: `<first 8 chars>***`. Lets
/// operators identify which token they have without ever showing
/// the full secret.
pub fn token_preview(raw: &str) -> String {
    let n = raw.chars().take(8).collect::<String>();
    format!("{n}***")
}

// ──────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_box() -> CryptoBox {
        // Deterministic key for test reproducibility — NOT a real key.
        let key = STANDARD.encode([0x42u8; 32]);
        CryptoBox::from_base64_key(&key).unwrap()
    }

    #[test]
    fn from_base64_rejects_short_key() {
        let short = STANDARD.encode([0u8; 16]);
        let err = CryptoBox::from_base64_key(&short).unwrap_err();
        assert!(err.to_string().contains("expected 32 bytes"));
    }

    #[test]
    fn from_base64_rejects_long_key() {
        let long = STANDARD.encode([0u8; 64]);
        let err = CryptoBox::from_base64_key(&long).unwrap_err();
        assert!(err.to_string().contains("expected 32 bytes"));
    }

    #[test]
    fn from_base64_rejects_invalid_base64() {
        let err = CryptoBox::from_base64_key("not valid base64 !!!!").unwrap_err();
        assert!(err.to_string().contains("not valid base64"));
    }

    #[test]
    fn round_trip_recovers_plaintext() {
        let cb = fixture_box();
        let nonce = CryptoBox::fresh_nonce();
        let plaintext = b"super-secret-runner-token-xyz";

        let ct = cb.encrypt(plaintext, &nonce).unwrap();
        let pt = cb.decrypt(&ct, &nonce).unwrap();

        assert_eq!(pt.expose(), std::str::from_utf8(plaintext).unwrap());
    }

    #[test]
    fn distinct_nonces_produce_distinct_ciphertexts() {
        // Reusing the same nonce under the same key would break the
        // AEAD security; verify our fresh_nonce() actually varies.
        let cb = fixture_box();
        let plaintext = b"same plaintext";
        let n1 = CryptoBox::fresh_nonce();
        let n2 = CryptoBox::fresh_nonce();
        assert_ne!(n1, n2, "fresh_nonce returned the same nonce twice");

        let ct1 = cb.encrypt(plaintext, &n1).unwrap();
        let ct2 = cb.encrypt(plaintext, &n2).unwrap();
        assert_ne!(ct1, ct2, "same plaintext + different nonces should yield different ciphertexts");
    }

    #[test]
    fn wrong_key_fails_decrypt() {
        let cb1 = fixture_box();
        let cb2 = CryptoBox::from_base64_key(&STANDARD.encode([0xFFu8; 32])).unwrap();
        let nonce = CryptoBox::fresh_nonce();
        let ct = cb1.encrypt(b"secret", &nonce).unwrap();

        let err = cb2.decrypt(&ct, &nonce).unwrap_err();
        assert!(
            err.to_string().contains("decrypt failed"),
            "wrong key should produce AEAD verification failure, got: {err}"
        );
    }

    #[test]
    fn tampered_ciphertext_fails_decrypt() {
        // AEAD must catch single-bit tampering — that's the whole point.
        let cb = fixture_box();
        let nonce = CryptoBox::fresh_nonce();
        let mut ct = cb.encrypt(b"secret", &nonce).unwrap();
        ct[0] ^= 0x01;

        let err = cb.decrypt(&ct, &nonce).unwrap_err();
        assert!(
            err.to_string().contains("decrypt failed"),
            "tampered ciphertext should fail AEAD verification, got: {err}"
        );
    }

    #[test]
    fn wrong_nonce_fails_decrypt() {
        let cb = fixture_box();
        let n1 = CryptoBox::fresh_nonce();
        let n2 = CryptoBox::fresh_nonce();
        let ct = cb.encrypt(b"secret", &n1).unwrap();

        let err = cb.decrypt(&ct, &n2).unwrap_err();
        assert!(err.to_string().contains("decrypt failed"));
    }

    #[test]
    fn nonce_length_validated_on_encrypt_and_decrypt() {
        let cb = fixture_box();
        let bad = [0u8; 8];
        assert!(cb.encrypt(b"x", &bad).is_err());
        assert!(cb.decrypt(b"y", &bad).is_err());
    }

    #[test]
    fn from_env_returns_none_when_unset() {
        // Save and clear; restore at end. Test runs single-threaded
        // by default in cargo test for this module so env-var swap
        // is safe enough for the lone read.
        // SAFETY: cargo test runs each test in its own thread but env
        // vars are process-global. This test could conflict with
        // other tests that also read REWIND_RUNNER_SECRET_KEY; we
        // accept that risk because none of the other crypto tests
        // touch this env var.
        let prev = std::env::var(KEY_ENV_VAR).ok();
        // SAFETY: setting / removing process env vars is unsafe in
        // multi-threaded contexts (rust 2024 edition). The test is
        // single-purpose and we restore state.
        unsafe {
            std::env::remove_var(KEY_ENV_VAR);
        }
        let cb = CryptoBox::from_env().unwrap();
        assert!(cb.is_none());

        // restore
        if let Some(v) = prev {
            unsafe {
                std::env::set_var(KEY_ENV_VAR, v);
            }
        }
    }

    #[test]
    fn generate_runner_token_is_url_safe_no_pad_and_unique() {
        let t1 = generate_runner_token();
        let t2 = generate_runner_token();
        assert_ne!(t1.expose(), t2.expose());
        // URL-safe base64-no-pad: only A-Za-z0-9_- characters.
        for c in t1.expose().chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "unexpected char in token: {c:?}"
            );
        }
    }

    #[test]
    fn hash_runner_token_is_stable_and_distinct() {
        let raw = "abc123";
        assert_eq!(hash_runner_token(raw), hash_runner_token(raw));
        assert_ne!(hash_runner_token(raw), hash_runner_token("abc124"));
        // Hex of length 64 (SHA-256).
        assert_eq!(hash_runner_token(raw).len(), 64);
    }

    #[test]
    fn token_preview_format() {
        assert_eq!(token_preview("abcdefghijklmn"), "abcdefgh***");
        // Short tokens still get a stable preview.
        assert_eq!(token_preview("abc"), "abc***");
    }

    #[test]
    fn sensitive_string_does_not_leak_in_debug() {
        // Make sure decrypt's return type genuinely redacts in Debug.
        let cb = fixture_box();
        let nonce = CryptoBox::fresh_nonce();
        let ct = cb.encrypt(b"super-secret", &nonce).unwrap();
        let pt = cb.decrypt(&ct, &nonce).unwrap();

        let dbg = format!("{pt:?}");
        assert!(
            !dbg.contains("super-secret"),
            "SensitiveString::Debug must not leak the plaintext, got: {dbg}"
        );
    }
}
