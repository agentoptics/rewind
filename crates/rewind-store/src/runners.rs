//! Runner registry + replay job tracking (Phase 3, commit 3/13).
//!
//! Three new tables, three Rust types:
//!
//! - **`runners`** ↔ [`Runner`]: registered agent processes that can
//!   accept replay-job webhooks. Stores name, mode (webhook/polling),
//!   webhook URL, encrypted auth token (under `REWIND_RUNNER_SECRET_KEY`),
//!   token hash for fast inbound auth lookup, status, last-seen timestamp.
//! - **`replay_jobs`** ↔ [`ReplayJob`]: dispatched replay jobs going
//!   through the state machine `pending → dispatched → in_progress →
//!   completed/errored`. Includes lease columns (`dispatch_deadline_at`,
//!   `lease_expires_at`) for the reaper task added in commit 5.
//! - **`replay_job_events`** ↔ [`ReplayJobEvent`]: append-only event log
//!   per job (started/progress/completed/errored). The dashboard's
//!   WebSocket re-broadcasts these.
//!
//! ## What this commit ships
//!
//! Pure data types + CRUD methods on [`Store`](crate::Store). No HTTP
//! endpoints (commit 4), no encryption logic (commit 4), no dispatcher
//! (commit 5). Keeping the storage-only piece reviewable on its own.
//!
//! ## Encryption boundary
//!
//! `Runner.encrypted_token` is **opaque bytes** to this module. The
//! `crypto` module added in commit 4 encrypts/decrypts via AES-256-GCM
//! under the app key. Runners.rs treats it as `Vec<u8>` and trusts
//! the caller (rewind-web's runner registration handler) to
//! encrypt before insert and decrypt at dispatch time.
//!
//! ## Token hash semantics
//!
//! `Runner.auth_token_hash` is `SHA-256(raw_token)` hex-encoded. Used
//! for the fast-path inbound-auth lookup: when a runner posts an event
//! with `X-Rewind-Runner-Auth: <token>`, the server hashes the supplied
//! value and looks up by `auth_token_hash` (indexed) — no decryption
//! needed in the hot path.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::Store;

// ── Runner ───────────────────────────────────────────────────────

/// How Rewind talks to a runner.
///
/// Phase 3 v1 ships only `Webhook` mode. The schema accommodates
/// `Polling` (NAT'd laptops) from day one so v3.1 can add it without
/// a migration; calls that supply `mode = Polling` today will return
/// `400 Bad Request` from the registration endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerMode {
    Webhook,
    Polling,
}

impl RunnerMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunnerMode::Webhook => "webhook",
            RunnerMode::Polling => "polling",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(RunnerMode::Webhook),
            "polling" => Some(RunnerMode::Polling),
            _ => None,
        }
    }
}

/// Runner lifecycle state.
///
/// `Active` = runner can receive jobs. `Disabled` = registration
/// exists but the operator has explicitly turned it off (e.g. for
/// maintenance). `Stale` = no heartbeat for >1h; reaper can flip
/// jobs targeted at stale runners to `errored` faster than the
/// general lease timeout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerStatus {
    Active,
    Disabled,
    Stale,
}

impl RunnerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunnerStatus::Active => "active",
            RunnerStatus::Disabled => "disabled",
            RunnerStatus::Stale => "stale",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(RunnerStatus::Active),
            "disabled" => Some(RunnerStatus::Disabled),
            "stale" => Some(RunnerStatus::Stale),
            _ => None,
        }
    }
}

/// A registered agent process that can receive replay-job webhooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runner {
    pub id: String,
    pub name: String,
    pub mode: RunnerMode,
    /// `None` for `mode = Polling`; required for `mode = Webhook`.
    pub webhook_url: Option<String>,
    /// AES-256-GCM-encrypted raw auth token. Encryption is the caller's
    /// responsibility (commit 4 `crypto` module). This module treats
    /// the bytes as opaque.
    pub encrypted_token: Vec<u8>,
    /// AES-GCM nonce (12 bytes) used to encrypt `encrypted_token`.
    /// Stored alongside the ciphertext; not secret in the AES-GCM
    /// threat model.
    pub token_nonce: Vec<u8>,
    /// `SHA-256(raw_token)` hex-encoded. Used for fast inbound auth
    /// lookup — when a runner sends `X-Rewind-Runner-Auth: <token>`,
    /// server hashes + looks up by this column (indexed).
    pub auth_token_hash: String,
    /// First 8 chars + `***` of the raw token. UI display only;
    /// lets operators identify which token they have without
    /// triggering the secret-redaction path.
    pub auth_token_preview: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub status: RunnerStatus,
}

// ── ReplayJob ────────────────────────────────────────────────────

/// State machine: `pending → dispatched → in_progress → completed/errored`.
///
/// **Cancellation is intentionally NOT in v1** (per Phase 3 plan
/// HIGH #5 resolution). v3.1 will add cooperative cancel with a
/// proper protocol; v1 jobs run to natural completion or error.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayJob {
    pub id: String,
    pub runner_id: String,
    pub session_id: String,
    pub replay_context_id: String,
    pub state: ReplayJobState,
    pub error_message: Option<String>,
    /// `"dispatch"` (runner didn't reply 202 by `dispatch_deadline_at`),
    /// `"agent"` (runner accepted but reported `errored`), or
    /// `"lease_expired"` (lease lapsed without progress events; reaper
    /// transitioned the job).
    pub error_stage: Option<String>,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Runner must reply 202 by this time or the reaper marks the job
    /// `errored` with `stage: "dispatch"`. Default: dispatched_at + 10s.
    pub dispatch_deadline_at: Option<DateTime<Utc>>,
    /// Extended on every heartbeat or progress event. Default:
    /// last_event_at + 5min. Reaper marks `errored` with
    /// `stage: "lease_expired"` when exceeded.
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub progress_step: u32,
    /// Runner-supplied total step count (optional). Useful for
    /// progress-bar UI; absent for streaming/unbounded runs.
    pub progress_total: Option<u32>,
}

// ── ReplayJobEvent ───────────────────────────────────────────────

/// A single event emitted by a runner during job execution. Append-only.
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
    /// Free-form JSON payload (kept as a string at the storage layer;
    /// the dashboard parses it). Typical shapes per event_type are
    /// documented in the Phase 3 plan's "Status events" section.
    pub payload: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ── CRUD on Store ────────────────────────────────────────────────

impl Store {
    // Runners
    // -------------------------------------------------------------

    /// Insert a new runner. Caller (rewind-web) is responsible for:
    /// 1. Generating the raw token + UUID id
    /// 2. Encrypting via the app key (AES-256-GCM)
    /// 3. Hashing for the auth_token_hash field
    /// 4. Calling this with the populated Runner row
    ///
    /// We keep the Runner→encryption boundary explicit (commit 4
    /// adds the encryption layer; this module stores opaque bytes).
    pub fn create_runner(&self, runner: &Runner) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runners (id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                runner.id,
                runner.name,
                runner.mode.as_str(),
                runner.webhook_url,
                runner.encrypted_token,
                runner.token_nonce,
                runner.auth_token_hash,
                runner.auth_token_preview,
                runner.created_at.to_rfc3339(),
                runner.last_seen_at.as_ref().map(|t| t.to_rfc3339()),
                runner.status.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Lookup by primary key.
    pub fn get_runner(&self, id: &str) -> Result<Option<Runner>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status
             FROM runners WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_runner)?;
        Ok(rows.next().transpose()?)
    }

    /// Fast-path inbound-auth lookup: given the SHA-256 hex of the
    /// runner-supplied `X-Rewind-Runner-Auth` header, find the matching
    /// runner. The `auth_token_hash` column is indexed.
    pub fn get_runner_by_auth_hash(&self, hash: &str) -> Result<Option<Runner>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status
             FROM runners WHERE auth_token_hash = ?1",
        )?;
        let mut rows = stmt.query_map(params![hash], Self::row_to_runner)?;
        Ok(rows.next().transpose()?)
    }

    /// List all runners ordered by created_at desc (newest first).
    pub fn list_runners(&self) -> Result<Vec<Runner>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status
             FROM runners ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], Self::row_to_runner)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Hard-delete a runner. Replay jobs referencing it remain in the
    /// DB (foreign-key constraint not cascading) so historical job
    /// records aren't silently lost; the dashboard's runner-detail
    /// view shows them as "Runner deleted".
    pub fn delete_runner(&self, id: &str) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM runners WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Update the runner's status (active / disabled / stale).
    pub fn set_runner_status(&self, id: &str, status: RunnerStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE runners SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id],
        )?;
        Ok(())
    }

    /// Update `last_seen_at` to `now`. Called from the heartbeat
    /// endpoint (commit 4).
    pub fn touch_runner_last_seen(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runners SET last_seen_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    fn row_to_runner(row: &rusqlite::Row) -> rusqlite::Result<Runner> {
        Ok(Runner {
            id: row.get(0)?,
            name: row.get(1)?,
            mode: {
                let s: String = row.get(2)?;
                RunnerMode::from_db_str(&s).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        format!("invalid runner mode: {s}").into(),
                    )
                })?
            },
            webhook_url: row.get(3)?,
            encrypted_token: row.get(4)?,
            token_nonce: row.get(5)?,
            auth_token_hash: row.get(6)?,
            auth_token_preview: row.get(7)?,
            created_at: parse_dt(row, 8)?,
            last_seen_at: parse_dt_opt(row, 9)?,
            status: {
                let s: String = row.get(10)?;
                RunnerStatus::from_db_str(&s).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        10,
                        rusqlite::types::Type::Text,
                        format!("invalid runner status: {s}").into(),
                    )
                })?
            },
        })
    }

    // ReplayJobs
    // -------------------------------------------------------------

    pub fn create_replay_job(&self, job: &ReplayJob) -> Result<()> {
        self.conn.execute(
            "INSERT INTO replay_jobs (id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                job.id,
                job.runner_id,
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
            ],
        )?;
        Ok(())
    }

    pub fn get_replay_job(&self, id: &str) -> Result<Option<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_replay_job)?;
        Ok(rows.next().transpose()?)
    }

    /// List jobs for a runner, newest first.
    pub fn list_replay_jobs_by_runner(&self, runner_id: &str, limit: u32) -> Result<Vec<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs WHERE runner_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![runner_id, limit], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List jobs for a session, newest first.
    pub fn list_replay_jobs_by_session(&self, session_id: &str) -> Result<Vec<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs WHERE session_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![session_id], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find expired jobs (dispatch deadline OR lease expired) that
    /// are still in non-terminal states. Used by the reaper task in
    /// commit 5.
    pub fn list_expired_replay_jobs(&self) -> Result<Vec<ReplayJob>> {
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
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

    /// Transition a job to a new state. Caller is responsible for
    /// validating the transition is legal — but we enforce
    /// terminal-state protection here: once a job is in a terminal
    /// state (`completed`/`errored`), this method refuses further
    /// transitions and returns false.
    ///
    /// Sets the corresponding timestamp column for the new state.
    pub fn advance_replay_job_state(
        &self,
        id: &str,
        new_state: ReplayJobState,
        error_message: Option<&str>,
        error_stage: Option<&str>,
    ) -> Result<bool> {
        let current = self.get_replay_job(id)?;
        let Some(job) = current else { return Ok(false); };
        if job.state.is_terminal() {
            // Refuse further transitions; idempotent caller behavior.
            return Ok(false);
        }
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
                    "UPDATE replay_jobs SET state = ?1, error_message = ?2, error_stage = ?3, {col} = ?4 WHERE id = ?5"
                ),
                params![new_state.as_str(), error_message, error_stage, val, id],
            )?
        } else {
            self.conn.execute(
                "UPDATE replay_jobs SET state = ?1, error_message = ?2, error_stage = ?3 WHERE id = ?4",
                params![new_state.as_str(), error_message, error_stage, id],
            )?
        };
        Ok(n > 0)
    }

    /// Extend the lease (called on heartbeat / progress events). The
    /// new `lease_expires_at` is computed by the caller (typically
    /// `Utc::now() + 5min`).
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

    /// Update progress counters. Called when a `progress` event arrives.
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
        })
    }

    // ReplayJobEvents (append-only)
    // -------------------------------------------------------------

    pub fn append_replay_job_event(&self, event: &ReplayJobEvent) -> Result<()> {
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

    /// List events for a job in insertion order (created_at asc).
    /// The dashboard's modal needs them in chronological order to
    /// render the progress timeline.
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
}

// ── Helpers ──────────────────────────────────────────────────────

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

    fn fake_runner(name: &str) -> Runner {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(format!("token-{name}").as_bytes());
        Runner {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            mode: RunnerMode::Webhook,
            webhook_url: Some("http://example.com/webhook".to_string()),
            encrypted_token: vec![1, 2, 3, 4],
            token_nonce: vec![0; 12],
            auth_token_hash: format!("{:x}", h.finalize()),
            auth_token_preview: "tok12345***".to_string(),
            created_at: Utc::now(),
            last_seen_at: None,
            status: RunnerStatus::Active,
        }
    }

    #[test]
    fn runner_round_trip() {
        let (store, _dir) = test_store();
        let runner = fake_runner("ray-agent");
        store.create_runner(&runner).unwrap();

        let fetched = store.get_runner(&runner.id).unwrap().unwrap();
        assert_eq!(fetched.name, "ray-agent");
        assert_eq!(fetched.mode, RunnerMode::Webhook);
        assert_eq!(fetched.encrypted_token, vec![1, 2, 3, 4]);
        assert_eq!(fetched.token_nonce, vec![0; 12]);
        assert_eq!(fetched.status, RunnerStatus::Active);
    }

    #[test]
    fn list_runners_orders_newest_first() {
        let (store, _dir) = test_store();
        let r1 = fake_runner("first");
        store.create_runner(&r1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let r2 = fake_runner("second");
        store.create_runner(&r2).unwrap();

        let list = store.list_runners().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "second");
        assert_eq!(list[1].name, "first");
    }

    #[test]
    fn lookup_by_auth_hash_is_indexed_path() {
        // Sanity that the indexed lookup returns the right runner. The
        // hash is unique per token; collision testing is the crypto
        // module's concern.
        let (store, _dir) = test_store();
        let runner = fake_runner("ray-agent");
        let hash = runner.auth_token_hash.clone();
        store.create_runner(&runner).unwrap();

        let found = store.get_runner_by_auth_hash(&hash).unwrap().unwrap();
        assert_eq!(found.id, runner.id);

        // Wrong hash → None.
        assert!(store.get_runner_by_auth_hash("nonexistent").unwrap().is_none());
    }

    #[test]
    fn delete_runner_returns_true_on_hit_false_on_miss() {
        let (store, _dir) = test_store();
        let runner = fake_runner("ephemeral");
        store.create_runner(&runner).unwrap();
        assert!(store.delete_runner(&runner.id).unwrap());
        // Second delete: row already gone.
        assert!(!store.delete_runner(&runner.id).unwrap());
        assert!(store.get_runner(&runner.id).unwrap().is_none());
    }

    #[test]
    fn set_runner_status_changes_active_to_disabled() {
        let (store, _dir) = test_store();
        let runner = fake_runner("toggle-me");
        store.create_runner(&runner).unwrap();
        assert_eq!(
            store.get_runner(&runner.id).unwrap().unwrap().status,
            RunnerStatus::Active
        );
        store.set_runner_status(&runner.id, RunnerStatus::Disabled).unwrap();
        assert_eq!(
            store.get_runner(&runner.id).unwrap().unwrap().status,
            RunnerStatus::Disabled
        );
    }

    #[test]
    fn touch_last_seen_updates_timestamp() {
        let (store, _dir) = test_store();
        let runner = fake_runner("heartbeat");
        store.create_runner(&runner).unwrap();
        assert!(store.get_runner(&runner.id).unwrap().unwrap().last_seen_at.is_none());

        store.touch_runner_last_seen(&runner.id).unwrap();
        let after = store.get_runner(&runner.id).unwrap().unwrap();
        assert!(after.last_seen_at.is_some());
    }

    fn fake_session_for_job(store: &Store) -> String {
        // Replay jobs FK to sessions, so we need a real session row.
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
        };
        store.create_session(&session).unwrap();
        session.id
    }

    fn fake_job(runner_id: &str, session_id: &str) -> ReplayJob {
        ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: runner_id.to_string(),
            session_id: session_id.to_string(),
            replay_context_id: Uuid::new_v4().to_string(),
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
        }
    }

    #[test]
    fn replay_job_round_trip() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);

        let job = fake_job(&runner.id, &session_id);
        store.create_replay_job(&job).unwrap();
        let fetched = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.state, ReplayJobState::Pending);
        assert_eq!(fetched.runner_id, runner.id);
        assert_eq!(fetched.session_id, session_id);
        assert_eq!(fetched.progress_step, 0);
    }

    #[test]
    fn advance_state_sets_correct_timestamp() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // pending → dispatched: dispatched_at set.
        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::Dispatched, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Dispatched);
        assert!(after.dispatched_at.is_some());

        // dispatched → in_progress: started_at set.
        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::InProgress, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.started_at.is_some());

        // in_progress → completed: completed_at set.
        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.completed_at.is_some());
    }

    #[test]
    fn advance_state_refuses_terminal_transitions() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // Move to errored.
        store
            .advance_replay_job_state(&job.id, ReplayJobState::Errored, Some("agent died"), Some("agent"))
            .unwrap();

        // Subsequent transition attempt: refused (returns false).
        // This protects against a runner that crashed and recovered
        // sending late events that would corrupt the terminal state.
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
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);

        // Job 1: in_progress with expired lease.
        let mut job1 = fake_job(&runner.id, &session_id);
        job1.state = ReplayJobState::InProgress;
        job1.lease_expires_at = Some(Utc::now() - chrono::Duration::seconds(60));
        store.create_replay_job(&job1).unwrap();

        // Job 2: in_progress with future lease — NOT expired.
        let mut job2 = fake_job(&runner.id, &session_id);
        job2.state = ReplayJobState::InProgress;
        job2.lease_expires_at = Some(Utc::now() + chrono::Duration::minutes(5));
        store.create_replay_job(&job2).unwrap();

        // Job 3: dispatched with expired dispatch deadline.
        let mut job3 = fake_job(&runner.id, &session_id);
        job3.state = ReplayJobState::Dispatched;
        job3.dispatch_deadline_at = Some(Utc::now() - chrono::Duration::seconds(30));
        store.create_replay_job(&job3).unwrap();

        // Job 4: terminal — must NOT appear in the expired list even
        // if its lease column happens to be in the past.
        let mut job4 = fake_job(&runner.id, &session_id);
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
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&runner.id, &session_id);
        job.state = ReplayJobState::InProgress;
        store.create_replay_job(&job).unwrap();

        // Active state — extend works.
        let new_lease = Utc::now() + chrono::Duration::minutes(10);
        store.extend_replay_job_lease(&job.id, new_lease).unwrap();
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.lease_expires_at.is_some());

        // Move to terminal; subsequent extend is a no-op (the
        // UPDATE statement filters by state).
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
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // Three events in a row.
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
        // Pin the terminal-state classification so a future state-
        // machine refactor doesn't accidentally let the reaper
        // overwrite a completed job.
        assert!(!ReplayJobState::Pending.is_terminal());
        assert!(!ReplayJobState::Dispatched.is_terminal());
        assert!(!ReplayJobState::InProgress.is_terminal());
        assert!(ReplayJobState::Completed.is_terminal());
        assert!(ReplayJobState::Errored.is_terminal());
    }
}
