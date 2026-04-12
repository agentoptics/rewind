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
    if full.len() <= 500 {
        Some(full)
    } else {
        Some(format!("{}...", &full[..500]))
    }
}

/// Check whether content blocks contain any tool_use blocks.
fn has_tool_use(content: &[ContentBlock]) -> bool {
    content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
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
        // Read transcript_path + byte offset from session metadata (brief lock)
        let (transcript_path, stored_offset) = {
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
            (tp, offset)
        };

        let transcript_path = match transcript_path {
            Some(p) => p,
            None => continue,
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

        if !new_lines.is_empty() {
            let sess_state = match state.hooks.sessions.get(claude_session_id) {
                Some(s) => s,
                None => continue,
            };

            let mut last_user_content: Option<serde_json::Value> = None;

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

                let content = match &message.content {
                    Some(c) if !c.is_empty() => c,
                    _ => continue,
                };

                // Only create LlmCall steps for entries with text content
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
                    if store.step_exists_by_tool_name(rewind_session_id, &dedup_key)? {
                        continue;
                    }
                }

                // Build response blob
                let thinking_preview = extract_thinking_preview(content);
                let response_obj = serde_json::json!({
                    "text": text,
                    "thinking_summary": thinking_preview,
                    "has_tool_use": has_tool_use(content),
                });

                let step_num = sess_state.step_counter.fetch_add(1, Ordering::Relaxed) + 1;
                let mut step = Step::new_llm_call(
                    &sess_state.timeline_id,
                    rewind_session_id,
                    step_num,
                    message.model.as_deref().unwrap_or(""),
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

                if !sess_state.root_span_id.is_empty() {
                    step.span_id = Some(sess_state.root_span_id.clone());
                }

                // Store blobs
                {
                    let store = match state.store.lock() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                    // Request blob: the preceding user message
                    if let Some(ref user_content) = last_user_content {
                        let req_bytes = serde_json::to_vec(user_content)?;
                        step.request_blob = store.blobs.put(&req_bytes)?;
                    }

                    // Response blob: the assistant text content
                    let resp_bytes = serde_json::to_vec(&response_obj)?;
                    step.response_blob = store.blobs.put(&resp_bytes)?;

                    store.create_step(&step)?;
                    store.update_session_stats(rewind_session_id, step_num, 0)?;
                }

                // Emit WebSocket event for live updates
                let _ = state.event_tx.send(StoreEvent::StepCreated {
                    session_id: rewind_session_id.clone(),
                    step: Box::new(step),
                });

                created += 1;
            }

            // Update the byte offset in metadata
            if new_offset != stored_offset {
                let store = match state.store.lock() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if let Ok(Some(session)) = store.get_session(rewind_session_id) {
                    let mut meta = session.metadata.clone();
                    meta["transcript_byte_offset"] = serde_json::json!(new_offset);
                    let _ = store.update_session_metadata(rewind_session_id, &meta);
                }
            }
        }

        // ── Token aggregation (full file re-read, same as before) ──

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

        {
            let store = match state.store.lock() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let session = match store.get_session(rewind_session_id) {
                Ok(Some(s)) => s,
                _ => continue,
            };

            let stored_cache = session
                .metadata
                .get("cache_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            if session.total_tokens != new_tokens || stored_cache != cache_tokens {
                if let Err(e) = store.update_session_tokens(rewind_session_id, new_tokens) {
                    tracing::debug!("Failed to update session tokens: {e}");
                    continue;
                }

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
    fn test_extract_thinking_preview_long() {
        let long_thinking = "x".repeat(1000);
        let blocks = vec![ContentBlock::Thinking {
            thinking: long_thinking,
        }];
        let preview = extract_thinking_preview(&blocks).unwrap();
        assert_eq!(preview.len(), 503); // 500 + "..."
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

    use chrono::Datelike;
}
