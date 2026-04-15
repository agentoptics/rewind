//! Transcript parsing for Claude Code JSONL transcript files.
//!
//! Claude Code writes a JSONL transcript to `~/.claude/projects/{project}/{session-id}.jsonl`.
//! Each line is a JSON object representing user prompts, assistant responses (with tool use and
//! text), system messages, etc.
//!
//! This module provides:
//! - Parsing of transcript files to extract aggregated token counts
//! - Creation of `LlmCall` steps for assistant text responses (not captured by hooks)
//! - Incremental reading via byte offset to avoid re-reading large files
//! - A background sync function that creates steps AND updates token data

use std::io::{BufRead, BufReader, Read as _, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::Ordering;

use chrono::{DateTime, Utc};
use rewind_store::{Step, StepStatus, StepType};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AppState, StoreEvent};

// ── Transcript entry types ──────────────────────────────────

/// Minimal envelope: just enough fields to detect the entry type and extract metadata.
/// The `message` field is kept as raw JSON because user entries and assistant entries
/// have incompatible `content` shapes (string vs array of content blocks).
#[derive(serde::Deserialize, Debug)]
struct TranscriptEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    /// Cursor extension uses `role` instead of `type`
    role: Option<String>,
    message: Option<serde_json::Value>,
    uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    #[allow(dead_code)]
    parent_uuid: Option<String>,
    timestamp: Option<String>,
}

impl TranscriptEntry {
    /// Unified accessor: returns entry type from either CLI or Cursor format.
    fn resolved_type(&self) -> Option<&str> {
        self.entry_type
            .as_deref()
            .or(self.role.as_deref())
    }

    /// Returns true if this is a user/human entry.
    fn is_user(&self) -> bool {
        matches!(self.resolved_type(), Some("user" | "human"))
    }

    /// Returns true if this is an assistant entry.
    fn is_assistant(&self) -> bool {
        self.resolved_type() == Some("assistant")
    }

    /// Try to parse the `message` field as an `AssistantMessage`.
    /// Only succeeds for assistant entries with the right structure.
    fn assistant_message(&self) -> Option<AssistantMessage> {
        self.message
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Deterministic ID for dedup: use the transcript UUID if present,
    /// otherwise derive one from a SHA-256 hash of the serialized entry.
    fn dedup_id(&self, raw_line: &str) -> String {
        if let Some(ref uuid) = self.uuid {
            return uuid.clone();
        }
        let mut hasher = Sha256::new();
        hasher.update(raw_line.as_bytes());
        let hash = hasher.finalize();
        format!("{:x}", hash)
    }

    /// Parse timestamp from the entry, falling back to `Utc::now()`.
    fn parsed_timestamp(&self) -> DateTime<Utc> {
        self.timestamp
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now)
    }
}

#[derive(serde::Deserialize, Debug)]
struct AssistantMessage {
    model: Option<String>,
    usage: Option<TokenUsage>,
    content: Option<Vec<ContentBlock>>,
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[allow(dead_code)]
        id: Option<String>,
        #[allow(dead_code)]
        name: Option<String>,
        #[allow(dead_code)]
        input: Option<serde_json::Value>,
    },
    #[serde(other)]
    Other,
}

#[derive(serde::Deserialize, Debug)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

// ── Aggregated result (still used for token totals) ─────────

struct TranscriptSummary {
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cache_read_tokens: u64,
    total_cache_creation_tokens: u64,
    model: Option<String>,
}

impl TranscriptSummary {
    fn new() -> Self {
        TranscriptSummary {
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
            model: None,
        }
    }

    /// New tokens only (input + output) — what you pay full price for.
    fn new_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// Cache tokens (read + creation) — served from prompt cache at 90% discount.
    fn cache_tokens(&self) -> u64 {
        self.total_cache_read_tokens + self.total_cache_creation_tokens
    }

    fn accumulate(&mut self, usage: &TokenUsage) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        self.total_cache_read_tokens += usage.cache_read_input_tokens;
        self.total_cache_creation_tokens += usage.cache_creation_input_tokens;
    }
}

// ── Incremental file reader ─────────────────────────────────

/// Read new lines from a file starting at `offset`.
/// Returns (lines, new_offset). If the file was truncated (new size < offset),
/// resets to 0 and reads everything.
///
/// The last line is only included if it ends with a newline (to avoid partial JSON).
fn read_new_lines(path: &Path, offset: u64) -> anyhow::Result<(Vec<String>, u64)> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("Failed to open transcript {}: {e}", path.display()))?;

    let file_size = file.metadata()?.len();

    // No new data
    if file_size == offset {
        return Ok((vec![], offset));
    }

    // File was truncated/rotated — reset to beginning
    let actual_offset = if file_size < offset { 0 } else { offset };
    file.seek(SeekFrom::Start(actual_offset))?;

    let mut raw = String::new();
    file.read_to_string(&mut raw)?;

    // Only process complete lines (ending with newline).
    // If the last chunk doesn't end with a newline, it's likely a partial write.
    let safe_end = if raw.ends_with('\n') {
        raw.len()
    } else {
        raw.rfind('\n').map(|i| i + 1).unwrap_or(0)
    };

    let consumed = &raw[..safe_end];
    let new_offset = actual_offset + safe_end as u64;

    let lines: Vec<String> = consumed
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok((lines, new_offset))
}

// ── Cursor transcript discovery ────────────────────────────

/// Attempt to find a Cursor IDE transcript file for the given session ID.
/// Searches `~/.cursor/projects/*/agent-transcripts/{session_id}/{session_id}.jsonl`.
/// Returns the path if found, None otherwise. Runs once per session.
fn discover_cursor_transcript(session_id: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let base = std::path::Path::new(&home).join(".cursor/projects");
    discover_cursor_transcript_in(session_id, &base)
}

/// Inner implementation that accepts a base path for testability.
fn discover_cursor_transcript_in(session_id: &str, base: &std::path::Path) -> Option<String> {
    if !base.is_dir() {
        return None;
    }

    let read_dir = std::fs::read_dir(base).ok()?;
    for entry in read_dir.flatten() {
        let candidate = entry.path()
            .join("agent-transcripts")
            .join(session_id)
            .join(format!("{}.jsonl", session_id));
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

// ── Content block helpers ───────────────────────────────────

/// Extract concatenated text from an assistant message's content blocks.
/// Returns None if there are no text blocks.
fn extract_text_content(content: &[ContentBlock]) -> Option<String> {
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

/// Extract a truncated thinking preview (first 500 chars) from thinking blocks.
fn extract_thinking_preview(content: &[ContentBlock]) -> Option<String> {
    let thinking: Vec<&str> = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Thinking { thinking } => Some(thinking.as_str()),
            _ => None,
        })
        .collect();

    if thinking.is_empty() {
        return None;
    }

    let full = thinking.join("\n");
    if full.chars().count() <= 500 {
        Some(full)
    } else {
        let truncated: String = full.chars().take(500).collect();
        Some(format!("{truncated}..."))
    }
}

/// Check whether content blocks contain any tool_use blocks.
fn has_tool_use(content: &[ContentBlock]) -> bool {
    content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
}

// ── Blob format helpers ─────────────────────────────────────
// The Web UI and API layer expect blobs in standard API formats:
// - Request: `{"messages": [{"role": "user", "content": "..."}], "model": "..."}`
// - Response: Anthropic format `{"content": [{"type": "text", "text": "..."}], ...}`

/// Build an API-format request blob from the preceding user transcript entry.
/// Extracts the user's text from various transcript formats.
fn build_request_blob(user_entry: &serde_json::Value, model: &str) -> serde_json::Value {
    let user_text = extract_user_text(user_entry);
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": user_text}]
    })
}

/// Extract the user's prompt text from a transcript JSONL entry.
/// Handles multiple formats:
/// - `{"message": {"content": "string"}}` (simple text)
/// - `{"message": {"content": [{"type": "text", "text": "..."}]}}` (content blocks)
/// - `{"content": "string"}` (flat format)
/// - `{"prompt": "string"}` (hook payload format)
fn extract_user_text(entry: &serde_json::Value) -> String {
    // Try message.content as string
    if let Some(text) = entry.pointer("/message/content").and_then(|c| c.as_str()) {
        return text.to_string();
    }
    // Try message.content as array of content blocks
    if let Some(blocks) = entry.pointer("/message/content").and_then(|c| c.as_array()) {
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    // Try top-level content
    if let Some(text) = entry.get("content").and_then(|c| c.as_str()) {
        return text.to_string();
    }
    // Try prompt field
    if let Some(text) = entry.get("prompt").and_then(|c| c.as_str()) {
        return text.to_string();
    }
    // Fallback: serialize the entry
    entry.to_string()
}

/// Build an Anthropic-format response blob from assistant content blocks.
fn build_response_blob(
    text: &str,
    thinking_preview: Option<&str>,
    has_tool_use: bool,
    model: &str,
    usage: Option<&TokenUsage>,
) -> serde_json::Value {
    let mut content = Vec::<serde_json::Value>::new();

    if let Some(tp) = thinking_preview {
        content.push(serde_json::json!({"type": "thinking", "thinking": tp}));
    }

    content.push(serde_json::json!({"type": "text", "text": text}));

    let mut resp = serde_json::json!({
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
    });

    if has_tool_use {
        resp["has_tool_use"] = serde_json::json!(true);
    }

    if let Some(u) = usage {
        resp["usage"] = serde_json::json!({
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
            "cache_read_input_tokens": u.cache_read_input_tokens,
            "cache_creation_input_tokens": u.cache_creation_input_tokens,
        });
    }

    resp
}

// ── Full-file parsing (for token aggregation) ───────────────

/// Parse a Claude Code JSONL transcript file and aggregate token usage.
/// Reads the entire file from the beginning.
fn parse_transcript(path: &Path) -> anyhow::Result<TranscriptSummary> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("Failed to open transcript {}: {e}", path.display()))?;

    let reader = BufReader::new(file);
    let mut summary = TranscriptSummary::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry: TranscriptEntry = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.is_assistant() {
            continue;
        }

        let message = match entry.assistant_message() {
            Some(m) => m,
            None => continue,
        };

        if summary.model.is_none() {
            summary.model = message.model;
        }

        if let Some(ref usage) = message.usage {
            summary.accumulate(usage);
        }
    }

    Ok(summary)
}

// ── Token backfill for stale sessions ──────────────────────

/// Attempt to backfill token totals for sessions that were auto-completed.
/// Reads `metadata.transcript_path` for each session ID and parses the
/// transcript file. Failures are logged and skipped (best-effort).
pub fn backfill_tokens(state: &crate::AppState, session_ids: &[String]) {
    for session_id in session_ids {
        let transcript_path = {
            let store = match state.store.lock() {
                Ok(s) => s,
                Err(_) => continue,
            };
            match store.get_session(session_id) {
                Ok(Some(s)) => s
                    .metadata
                    .get("transcript_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                _ => continue,
            }
        };

        let transcript_path = match transcript_path {
            Some(p) => p,
            None => continue,
        };

        let path = std::path::Path::new(&transcript_path);
        if !path.exists() {
            continue;
        }

        let summary = match parse_transcript(path) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Token backfill skipped for {session_id}: {e}");
                continue;
            }
        };

        let new_tokens = summary.new_tokens();
        let cache_tokens = summary.cache_tokens();
        if new_tokens == 0 && cache_tokens == 0 {
            continue;
        }

        let store = match state.store.lock() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(Some(session)) = store.get_session(session_id)
            && session.total_tokens < new_tokens
        {
            let _ = store.update_session_tokens(session_id, new_tokens);
            let mut meta = session.metadata.clone();
            meta["cache_tokens"] = serde_json::json!(cache_tokens);
            if let Some(ref model) = summary.model {
                meta["model"] = serde_json::json!(model);
            }
            let _ = store.update_session_metadata(session_id, &meta);
            tracing::debug!("Backfilled tokens for {session_id}: {new_tokens} new, {cache_tokens} cached");
        }
    }
}

// ── Sync function: steps + tokens ───────────────────────────

/// Iterate over all active hook sessions, read their transcript files,
/// create `LlmCall` steps for assistant text responses, and update token totals.
///
/// This replaces the old `sync_transcript_tokens` — it both creates steps
/// AND aggregates token data in a single pass.
///
/// Returns the number of new LlmCall steps created.
pub fn sync_transcript_steps(state: &AppState) -> anyhow::Result<usize> {
    let mut created = 0usize;

    let sessions_to_check: Vec<(String, String)> = {
        state
            .hooks
            .sessions
            .iter()
            .map(|entry| {
                let claude_id = entry.key().clone();
                let rewind_id = entry.value().session_id.clone();
                (rewind_id, claude_id)
            })
            .collect()
    };

    for (rewind_session_id, claude_session_id) in &sessions_to_check {
        // Read transcript_path, byte offset, hook_source, and discovery state from session metadata (brief lock)
        let (transcript_path, stored_offset, hook_source, discovery_attempted) = {
            let store = match state.store.lock() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let session = match store.get_session(rewind_session_id) {
                Ok(Some(s)) => s,
                _ => continue,
            };
            let tp = session
                .metadata
                .get("transcript_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let offset = session
                .metadata
                .get("transcript_byte_offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let src = session
                .metadata
                .get("hook_source")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let discovery_attempted = session
                .metadata
                .get("transcript_discovery_attempted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            (tp, offset, src, discovery_attempted)
        };

        let transcript_path = match transcript_path {
            Some(p) => p,
            None => {
                // For Cursor sessions, try to discover the transcript file on disk.
                // Skip if not a Cursor session or if discovery was already attempted.
                if hook_source.as_deref() != Some("cursor") || discovery_attempted {
                    continue;
                }

                match discover_cursor_transcript(claude_session_id) {
                    Some(discovered) => {
                        // Persist the discovered path so we don't scan again
                        if let Ok(store) = state.store.lock()
                            && let Ok(Some(session)) = store.get_session(rewind_session_id)
                        {
                            let mut meta = session.metadata.clone();
                            meta["transcript_path"] = serde_json::json!(&discovered);
                            let _ = store.update_session_metadata(rewind_session_id, &meta);
                        }
                        tracing::info!(
                            "Discovered Cursor transcript for session {}: {}",
                            &claude_session_id[..8.min(claude_session_id.len())],
                            &discovered
                        );
                        discovered
                    }
                    None => {
                        // Cache the failure so we don't re-scan every sync cycle
                        if let Ok(store) = state.store.lock()
                            && let Ok(Some(session)) = store.get_session(rewind_session_id)
                        {
                            let mut meta = session.metadata.clone();
                            meta["transcript_discovery_attempted"] = serde_json::json!(true);
                            let _ = store.update_session_metadata(rewind_session_id, &meta);
                        }
                        continue;
                    }
                }
            }
        };

        let path = Path::new(&transcript_path);
        if !path.exists() {
            continue;
        }

        // ── Incremental step creation from new lines ──

        let (new_lines, new_offset) = match read_new_lines(path, stored_offset) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("Transcript read error for {}: {e}", transcript_path);
                continue;
            }
        };

        // Clone session state fields upfront and drop the DashMap guard
        // to avoid holding it across the entire line-processing loop (#5).
        let (timeline_id, root_span_id, step_counter) = match state.hooks.sessions.get(claude_session_id) {
            Some(s) => (
                s.timeline_id.clone(),
                s.root_span_id.clone(),
                std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
                    s.step_counter.load(Ordering::Relaxed),
                )),
            ),
            None => continue,
        };
        // Ref guard is now dropped

        if !new_lines.is_empty() {
            let mut last_user_content: Option<serde_json::Value> = None;
            let mut summary = TranscriptSummary::new();

            for line in &new_lines {
                let trimmed = line.trim();
                let entry: TranscriptEntry = match serde_json::from_str(trimmed) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                // Track user entries as context for the next assistant response
                if entry.is_user() {
                    last_user_content = serde_json::from_str::<serde_json::Value>(trimmed).ok();
                    continue;
                }

                if !entry.is_assistant() {
                    continue;
                }

                let message = match entry.assistant_message() {
                    Some(m) => m,
                    None => continue,
                };

                // Accumulate tokens in the incremental loop (#6)
                if let Some(ref usage) = message.usage {
                    summary.accumulate(usage);
                }
                if summary.model.is_none() {
                    summary.model = message.model.clone();
                }

                let content = match &message.content {
                    Some(c) if !c.is_empty() => c,
                    _ => continue,
                };

                let text = match extract_text_content(content) {
                    Some(t) => t,
                    None => continue,
                };

                // Dedup: check if this transcript entry was already processed
                let dedup_key = format!("transcript:{}", entry.dedup_id(trimmed));
                {
                    let store = match state.store.lock() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    // Use match+continue instead of ? to avoid aborting sync for all sessions (#2)
                    match store.step_exists_by_tool_name(rewind_session_id, &dedup_key) {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(e) => {
                            tracing::debug!("Dedup check failed for {}: {e}", rewind_session_id);
                            continue;
                        }
                    }
                }

                let model_str = message.model.as_deref().unwrap_or("");
                let thinking_preview = extract_thinking_preview(content);
                let content_has_tool_use = has_tool_use(content);

                let response_obj = build_response_blob(
                    &text,
                    thinking_preview.as_deref(),
                    content_has_tool_use,
                    model_str,
                    message.usage.as_ref(),
                );

                // Build step WITHOUT incrementing counter yet (#3)
                let mut step = Step::new_llm_call(
                    &timeline_id,
                    rewind_session_id,
                    0, // placeholder — set after successful create
                    model_str,
                );
                step.id = Uuid::new_v4().to_string();
                step.step_type = StepType::LlmCall;
                step.status = StepStatus::Success;
                step.tool_name = Some(dedup_key);
                step.created_at = entry.parsed_timestamp();

                if let Some(ref usage) = message.usage {
                    step.tokens_in = usage.input_tokens;
                    step.tokens_out = usage.output_tokens;
                }

                if !root_span_id.is_empty() {
                    step.span_id = Some(root_span_id.clone());
                }

                // Store blobs + create step — all errors use continue, not ? (#2)
                let persist_ok = (|| -> anyhow::Result<u32> {
                    let store = state.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;

                    if let Some(ref user_content) = last_user_content {
                        let req_obj = build_request_blob(user_content, model_str);
                        let req_bytes = serde_json::to_vec(&req_obj)?;
                        step.request_blob = store.blobs.put(&req_bytes)?;
                    }

                    let resp_bytes = serde_json::to_vec(&response_obj)?;
                    step.response_blob = store.blobs.put(&resp_bytes)?;

                    // Increment counter only after blobs succeed, right before create_step (#3)
                    let step_num = step_counter.fetch_add(1, Ordering::Relaxed) + 1;
                    step.step_number = step_num;

                    store.create_step(&step)?;
                    store.update_session_stats(rewind_session_id, step_num, 0)?;
                    Ok(step_num)
                })();

                match persist_ok {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::debug!("Failed to persist transcript step: {e}");
                        continue;
                    }
                }

                // Sync the authoritative counter back to the DashMap (#3)
                if let Some(sess_state) = state.hooks.sessions.get(claude_session_id) {
                    let local_val = step_counter.load(Ordering::Relaxed);
                    sess_state.step_counter.fetch_max(local_val, Ordering::Relaxed);
                }

                let _ = state.event_tx.send(StoreEvent::StepCreated {
                    session_id: rewind_session_id.clone(),
                    step: Box::new(step),
                });

                created += 1;
            }

            // Update byte offset + token totals in one metadata write
            {
                let store = match state.store.lock() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if let Ok(Some(session)) = store.get_session(rewind_session_id) {
                    let mut meta = session.metadata.clone();

                    if new_offset != stored_offset {
                        meta["transcript_byte_offset"] = serde_json::json!(new_offset);
                    }

                    // Token aggregation from incremental pass (#6)
                    let new_tokens = summary.new_tokens();
                    let cache_tokens = summary.cache_tokens();
                    if new_tokens > 0 || cache_tokens > 0 {
                        let _ = store.update_session_tokens(rewind_session_id, new_tokens);
                        meta["cache_tokens"] = serde_json::json!(cache_tokens);
                        if let Some(ref model) = summary.model {
                            meta["model"] = serde_json::json!(model);
                        }

                        let _ = state.event_tx.send(StoreEvent::SessionUpdated {
                            session_id: rewind_session_id.clone(),
                            status: session.status.as_str().to_string(),
                            total_steps: session.total_steps,
                            total_tokens: new_tokens,
                        });
                    }

                    let _ = store.update_session_metadata(rewind_session_id, &meta);
                }
            }
        } else {
            // No new lines — but still need token aggregation for sessions
            // that started before the server (offset was 0, full file already read).
            // Only re-read when offset is 0 (first sync); afterward the incremental
            // loop handles it.
            if stored_offset == 0 {
                let summary = match parse_transcript(path) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("Transcript parse error for {}: {e}", transcript_path);
                        continue;
                    }
                };

                let new_tokens = summary.new_tokens();
                let cache_tokens = summary.cache_tokens();
                if new_tokens == 0 && cache_tokens == 0 {
                    continue;
                }

                let store = match state.store.lock() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if let Ok(Some(session)) = store.get_session(rewind_session_id) {
                    let stored_cache = session.metadata.get("cache_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    if session.total_tokens != new_tokens || stored_cache != cache_tokens {
                        let _ = store.update_session_tokens(rewind_session_id, new_tokens);
                        let mut meta = session.metadata.clone();
                        meta["cache_tokens"] = serde_json::json!(cache_tokens);
                        if let Some(ref model) = summary.model {
                            meta["model"] = serde_json::json!(model);
                        }
                        let _ = store.update_session_metadata(rewind_session_id, &meta);
                        let _ = state.event_tx.send(StoreEvent::SessionUpdated {
                            session_id: rewind_session_id.clone(),
                            status: session.status.as_str().to_string(),
                            total_steps: session.total_steps,
                            total_tokens: new_tokens,
                        });
                    }
                }
            }
        }
    }

    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── Token aggregation tests (existing) ──────────────────

    #[test]
    fn test_parse_transcript_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();

        writeln!(file, r#"{{"type":"human","message":{{"content":"hello"}},"timestamp":"2026-04-11T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"claude-opus-4-6","usage":{{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":200,"cache_creation_input_tokens":1000}},"stop_reason":"tool_use","content":[]}},"timestamp":"2026-04-11T00:00:01Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"claude-opus-4-6","usage":{{"input_tokens":150,"output_tokens":75,"cache_read_input_tokens":300,"cache_creation_input_tokens":0}},"stop_reason":"end_turn","content":[]}},"timestamp":"2026-04-11T00:00:02Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"claude-opus-4-6","usag"#).unwrap();

        let summary = parse_transcript(&path).unwrap();
        assert_eq!(summary.total_input_tokens, 250);
        assert_eq!(summary.total_output_tokens, 125);
        assert_eq!(summary.total_cache_read_tokens, 500);
        assert_eq!(summary.total_cache_creation_tokens, 1000);
        assert_eq!(summary.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(summary.new_tokens(), 375);
        assert_eq!(summary.cache_tokens(), 1500);
    }

    #[test]
    fn test_parse_transcript_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::File::create(&path).unwrap();

        let summary = parse_transcript(&path).unwrap();
        assert_eq!(summary.new_tokens(), 0);
        assert!(summary.model.is_none());
    }

    #[test]
    fn test_parse_transcript_no_assistant_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_assistant.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"human","message":{{"content":"hello"}},"timestamp":"2026-04-11T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"system","message":{{"content":"init"}},"timestamp":"2026-04-11T00:00:00Z"}}"#).unwrap();

        let summary = parse_transcript(&path).unwrap();
        assert_eq!(summary.new_tokens(), 0);
        assert!(summary.model.is_none());
    }

    #[test]
    fn test_parse_transcript_missing_file() {
        let result = parse_transcript(Path::new("/nonexistent/path/foo.jsonl"));
        assert!(result.is_err());
    }

    // ── Incremental reader tests ────────────────────────────

    #[test]
    fn test_read_new_lines_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"user"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant"}}"#).unwrap();

        let (lines, offset) = read_new_lines(&path, 0).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(offset > 0);
    }

    #[test]
    fn test_read_new_lines_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"user"}}"#).unwrap();
        drop(file);

        let (lines1, offset1) = read_new_lines(&path, 0).unwrap();
        assert_eq!(lines1.len(), 1);

        // Append more data
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, r#"{{"type":"assistant"}}"#).unwrap();
        drop(file);

        let (lines2, offset2) = read_new_lines(&path, offset1).unwrap();
        assert_eq!(lines2.len(), 1);
        assert!(offset2 > offset1);
        assert!(lines2[0].contains("assistant"));
    }

    #[test]
    fn test_read_new_lines_no_new_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"user"}}"#).unwrap();
        drop(file);

        let (_, offset) = read_new_lines(&path, 0).unwrap();
        let (lines, offset2) = read_new_lines(&path, offset).unwrap();
        assert_eq!(lines.len(), 0);
        assert_eq!(offset, offset2);
    }

    #[test]
    fn test_read_new_lines_truncated_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"user"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant"}}"#).unwrap();
        drop(file);

        // Simulate file truncation (new session)
        std::fs::write(&path, "{\"type\":\"user\"}\n").unwrap();

        // Old offset is larger than new file — should reset to 0
        let (lines, _offset) = read_new_lines(&path, 9999).unwrap();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_read_new_lines_partial_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"user"}}"#).unwrap();
        // Write a partial line (no trailing newline)
        write!(file, r#"{{"type":"assistant","message":{{"mod"#).unwrap();
        drop(file);

        let (lines, _) = read_new_lines(&path, 0).unwrap();
        // Should only return the complete first line
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("user"));
    }

    // ── Entry type detection tests ──────────────────────────

    #[test]
    fn test_entry_type_cli_format() {
        let entry: TranscriptEntry = serde_json::from_str(
            r#"{"type":"assistant","message":{"model":"claude-3","content":[{"type":"text","text":"hello"}]},"uuid":"abc-123","timestamp":"2026-04-11T00:00:00Z"}"#
        ).unwrap();
        assert!(entry.is_assistant());
        assert!(!entry.is_user());
        assert_eq!(entry.dedup_id(""), "abc-123");
    }

    #[test]
    fn test_entry_type_cursor_format() {
        let entry: TranscriptEntry = serde_json::from_str(
            r#"{"role":"assistant","message":{"model":"claude-3","content":[{"type":"text","text":"hello"}]}}"#
        ).unwrap();
        assert!(entry.is_assistant());
        assert_eq!(entry.resolved_type(), Some("assistant"));
    }

    #[test]
    fn test_entry_type_user_variants() {
        let user_cli: TranscriptEntry = serde_json::from_str(r#"{"type":"user"}"#).unwrap();
        assert!(user_cli.is_user());

        let user_human: TranscriptEntry = serde_json::from_str(r#"{"type":"human"}"#).unwrap();
        assert!(user_human.is_user());

        let user_cursor: TranscriptEntry = serde_json::from_str(r#"{"role":"user"}"#).unwrap();
        assert!(user_cursor.is_user());
    }

    #[test]
    fn test_dedup_id_without_uuid() {
        let entry: TranscriptEntry = serde_json::from_str(r#"{"role":"assistant"}"#).unwrap();
        let id = entry.dedup_id(r#"{"role":"assistant"}"#);
        assert!(!id.is_empty());
        // Deterministic: same input → same hash
        let id2 = entry.dedup_id(r#"{"role":"assistant"}"#);
        assert_eq!(id, id2);
    }

    #[test]
    fn test_parsed_timestamp_valid() {
        let entry: TranscriptEntry = serde_json::from_str(
            r#"{"type":"assistant","timestamp":"2026-04-11T10:30:00Z"}"#
        ).unwrap();
        let ts = entry.parsed_timestamp();
        assert_eq!(ts.year(), 2026);
    }

    #[test]
    fn test_parsed_timestamp_missing() {
        let entry: TranscriptEntry = serde_json::from_str(r#"{"type":"assistant"}"#).unwrap();
        let ts = entry.parsed_timestamp();
        // Should fall back to now — just check it's recent
        assert!(ts.year() >= 2026);
    }

    // ── Content block extraction tests ──────────────────────

    #[test]
    fn test_extract_text_content_pure_text() {
        let blocks = vec![
            ContentBlock::Text { text: "Hello".to_string() },
            ContentBlock::Text { text: "World".to_string() },
        ];
        let text = extract_text_content(&blocks).unwrap();
        assert_eq!(text, "Hello\n\nWorld");
    }

    #[test]
    fn test_extract_text_content_only_tool_use() {
        let blocks = vec![ContentBlock::ToolUse {
            id: Some("id1".into()),
            name: Some("Read".into()),
            input: None,
        }];
        assert!(extract_text_content(&blocks).is_none());
    }

    #[test]
    fn test_extract_text_content_mixed() {
        let blocks = vec![
            ContentBlock::Text { text: "Let me read that file.".to_string() },
            ContentBlock::ToolUse { id: Some("id1".into()), name: Some("Read".into()), input: None },
        ];
        let text = extract_text_content(&blocks).unwrap();
        assert_eq!(text, "Let me read that file.");
        assert!(has_tool_use(&blocks));
    }

    #[test]
    fn test_extract_thinking_preview_short() {
        let blocks = vec![ContentBlock::Thinking {
            thinking: "Short thought".to_string(),
        }];
        let preview = extract_thinking_preview(&blocks).unwrap();
        assert_eq!(preview, "Short thought");
    }

    #[test]
    fn test_extract_thinking_preview_long_ascii() {
        let long_thinking = "x".repeat(1000);
        let blocks = vec![ContentBlock::Thinking {
            thinking: long_thinking,
        }];
        let preview = extract_thinking_preview(&blocks).unwrap();
        assert_eq!(preview.len(), 503); // 500 + "..."
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn test_extract_thinking_preview_multibyte_safe() {
        // Each CJK char is 3 bytes in UTF-8. 500 chars = 1500 bytes.
        // The old byte-slice approach would panic at a non-char boundary.
        let cjk_thinking = "思".repeat(1000);
        let blocks = vec![ContentBlock::Thinking {
            thinking: cjk_thinking,
        }];
        let preview = extract_thinking_preview(&blocks).unwrap();
        assert_eq!(preview.chars().count(), 503); // 500 chars + "..."
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn test_has_tool_use() {
        let with_tool = vec![
            ContentBlock::Text { text: "hi".into() },
            ContentBlock::ToolUse { id: None, name: None, input: None },
        ];
        assert!(has_tool_use(&with_tool));

        let without_tool = vec![ContentBlock::Text { text: "hi".into() }];
        assert!(!has_tool_use(&without_tool));
    }

    // ── Full content block deserialization test ──────────────

    #[test]
    fn test_deserialize_full_assistant_entry() {
        let json = r#"{
            "type": "assistant",
            "uuid": "test-uuid-123",
            "timestamp": "2026-04-11T12:00:00Z",
            "message": {
                "model": "claude-opus-4-6",
                "usage": {
                    "input_tokens": 500,
                    "output_tokens": 200,
                    "cache_read_input_tokens": 100,
                    "cache_creation_input_tokens": 50
                },
                "content": [
                    {"type": "thinking", "thinking": "Let me analyze this..."},
                    {"type": "text", "text": "Here is my analysis."},
                    {"type": "tool_use", "id": "tu_1", "name": "Read", "input": {"path": "/foo"}}
                ],
                "stop_reason": "tool_use"
            }
        }"#;

        let entry: TranscriptEntry = serde_json::from_str(json).unwrap();
        assert!(entry.is_assistant());
        assert_eq!(entry.uuid.as_deref(), Some("test-uuid-123"));

        let msg = entry.assistant_message().unwrap();
        assert_eq!(msg.model.as_deref(), Some("claude-opus-4-6"));

        let content = msg.content.as_ref().unwrap();
        assert_eq!(content.len(), 3);

        let text = extract_text_content(content).unwrap();
        assert_eq!(text, "Here is my analysis.");

        let thinking = extract_thinking_preview(content).unwrap();
        assert_eq!(thinking, "Let me analyze this...");

        assert!(has_tool_use(content));

        let usage = msg.usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 200);
    }

    #[test]
    fn test_skip_tool_use_only_entries() {
        let json = r#"{
            "type": "assistant",
            "message": {
                "model": "claude-opus-4-6",
                "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "Read", "input": {"path": "/foo"}}
                ]
            }
        }"#;

        let entry: TranscriptEntry = serde_json::from_str(json).unwrap();
        let msg = entry.assistant_message().unwrap();
        let content = msg.content.as_ref().unwrap();
        // No text → should not create LlmCall step
        assert!(extract_text_content(content).is_none());
    }

    // ── Blob format helper tests ──────────────────────────────

    #[test]
    fn test_extract_user_text_string_content() {
        let entry = serde_json::json!({"type":"user","message":{"content":"What is Rust?"}});
        assert_eq!(extract_user_text(&entry), "What is Rust?");
    }

    #[test]
    fn test_extract_user_text_content_blocks() {
        let entry = serde_json::json!({
            "type":"user",
            "message":{"content":[{"type":"text","text":"Hello"},{"type":"text","text":"World"}]}
        });
        assert_eq!(extract_user_text(&entry), "Hello\nWorld");
    }

    #[test]
    fn test_extract_user_text_flat_format() {
        let entry = serde_json::json!({"content": "Flat prompt"});
        assert_eq!(extract_user_text(&entry), "Flat prompt");
    }

    #[test]
    fn test_build_request_blob_format() {
        let user_entry = serde_json::json!({"type":"user","message":{"content":"Hello"}});
        let blob = build_request_blob(&user_entry, "claude-opus-4-6");
        assert_eq!(blob["model"], "claude-opus-4-6");
        let messages = blob["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn test_build_response_blob_format() {
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let blob = build_response_blob("Answer here", Some("I thought..."), true, "claude-opus-4-6", Some(&usage));

        assert_eq!(blob["type"], "message");
        assert_eq!(blob["role"], "assistant");
        assert_eq!(blob["model"], "claude-opus-4-6");
        assert_eq!(blob["has_tool_use"], true);

        let content = blob["content"].as_array().unwrap();
        assert_eq!(content.len(), 2); // thinking + text

        let thinking = content.iter().find(|b| b["type"] == "thinking").unwrap();
        assert_eq!(thinking["thinking"], "I thought...");

        let text = content.iter().find(|b| b["type"] == "text").unwrap();
        assert_eq!(text["text"], "Answer here");

        assert_eq!(blob["usage"]["input_tokens"], 100);
        assert_eq!(blob["usage"]["output_tokens"], 50);
    }

    #[test]
    fn test_build_response_blob_no_thinking() {
        let blob = build_response_blob("Just text", None, false, "claude-3", None);
        let content = blob["content"].as_array().unwrap();
        assert_eq!(content.len(), 1); // text only
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Just text");
        assert!(blob.get("has_tool_use").is_none());
    }

    use chrono::Datelike;

    // ── Cursor transcript discovery tests ───────────────────

    #[test]
    fn test_discover_cursor_transcript_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_id = "abc-def-123";

        // Create the expected directory structure:
        // {base}/1234567890/agent-transcripts/abc-def-123/abc-def-123.jsonl
        let transcript_dir = tmp.path()
            .join("1234567890")
            .join("agent-transcripts")
            .join(session_id);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let transcript_file = transcript_dir.join(format!("{}.jsonl", session_id));
        std::fs::write(&transcript_file, "{}\n").unwrap();

        let result = super::discover_cursor_transcript_in(session_id, tmp.path());
        assert!(result.is_some(), "Should discover transcript file");
        assert_eq!(result.unwrap(), transcript_file.to_string_lossy().to_string());
    }

    #[test]
    fn test_discover_cursor_transcript_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Create a project dir but no matching transcript
        std::fs::create_dir_all(tmp.path().join("1234567890/agent-transcripts/other-session")).unwrap();

        let result = super::discover_cursor_transcript_in("nonexistent-session", tmp.path());
        assert!(result.is_none(), "Should return None when transcript not found");
    }

    #[test]
    fn test_discover_cursor_transcript_no_base_dir() {
        let result = super::discover_cursor_transcript_in(
            "any-session",
            std::path::Path::new("/nonexistent/path"),
        );
        assert!(result.is_none(), "Should return None when base dir doesn't exist");
    }

    #[test]
    fn test_discover_cursor_transcript_multiple_projects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_id = "target-session";

        // Create multiple project directories, only one has the transcript
        std::fs::create_dir_all(tmp.path().join("111/agent-transcripts/other")).unwrap();
        std::fs::create_dir_all(tmp.path().join("222/agent-transcripts")).unwrap();

        let transcript_dir = tmp.path()
            .join("333")
            .join("agent-transcripts")
            .join(session_id);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(transcript_dir.join(format!("{}.jsonl", session_id)), "{}\n").unwrap();

        let result = super::discover_cursor_transcript_in(session_id, tmp.path());
        assert!(result.is_some(), "Should find transcript across multiple project dirs");
    }
}
