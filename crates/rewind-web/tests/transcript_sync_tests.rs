use rewind_store::*;
use rewind_web::{AppState, HookIngestionState, StoreEvent};
use std::io::Write;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn setup() -> (AppState, Arc<Mutex<Store>>, TempDir, Arc<HookIngestionState>) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(64);
    let hooks = Arc::new(HookIngestionState::new());
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: hooks.clone(),
        otel_config: None,
    };
    (state, store, tmp, hooks)
}

/// Create a session + timeline in the store and register it in the hooks DashMap.
/// Returns the rewind session_id.
fn create_hook_session(
    state: &AppState,
    claude_session_id: &str,
    transcript_path: &str,
) -> String {
    let mut session = Session::new("test-session");
    session.source = SessionSource::Hooks;
    session.metadata = serde_json::json!({
        "claude_session_id": claude_session_id,
        "transcript_path": transcript_path,
    });

    let timeline = Timeline::new_root(&session.id);
    let rewind_session_id = session.id.clone();
    let timeline_id = timeline.id.clone();

    {
        let s = state.store.lock().unwrap();
        s.create_session(&session).unwrap();
        s.create_timeline(&timeline).unwrap();
    }

    state.hooks.sessions.insert(
        claude_session_id.to_string(),
        rewind_web::hooks::HookSessionState {
            session_id: rewind_session_id.clone(),
            timeline_id,
            root_span_id: String::new(),
            step_counter: AtomicU32::new(0),
            pending_steps: Mutex::new(std::collections::HashMap::new()),
        },
    );

    rewind_session_id
}

fn write_transcript(path: &std::path::Path, lines: &[&str]) {
    let mut file = std::fs::File::create(path).unwrap();
    for line in lines {
        writeln!(file, "{}", line).unwrap();
    }
}

fn append_transcript(path: &std::path::Path, lines: &[&str]) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for line in lines {
        writeln!(file, "{}", line).unwrap();
    }
}

// ── Test 1: Basic LlmCall step creation from transcript ──

#[tokio::test]
async fn test_sync_creates_llm_call_steps() {
    let (state, store, _tmp, _hooks) = setup();
    let transcript_dir = TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");

    write_transcript(
        &transcript_path,
        &[
            r#"{"type":"user","message":{"content":"What is Rust?"},"timestamp":"2026-04-11T10:00:00Z"}"#,
            r#"{"type":"assistant","uuid":"resp-1","message":{"model":"claude-opus-4-6","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"Rust is a systems programming language."}]},"timestamp":"2026-04-11T10:00:01Z"}"#,
        ],
    );

    let session_id = create_hook_session(
        &state,
        "claude-sess-1",
        transcript_path.to_str().unwrap(),
    );

    let created = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created, 1);

    let s = store.lock().unwrap();
    let sess = s.get_session(&session_id).unwrap().unwrap();
    let timeline = s.get_root_timeline(&session_id).unwrap().unwrap();
    let steps = s.get_steps(&timeline.id).unwrap();

    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].step_type, StepType::LlmCall);
    assert_eq!(steps[0].status, StepStatus::Success);
    assert_eq!(steps[0].model, "claude-opus-4-6");
    assert_eq!(steps[0].tokens_in, 100);
    assert_eq!(steps[0].tokens_out, 50);
    assert!(steps[0].tool_name.as_ref().unwrap().starts_with("transcript:resp-1"));

    // Response blob should be in Anthropic format
    let resp_bytes = s.blobs.get(&steps[0].response_blob).unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(resp["role"], "assistant");
    assert_eq!(resp["type"], "message");
    let content = resp["content"].as_array().unwrap();
    let text_block = content.iter().find(|b| b["type"] == "text").unwrap();
    assert_eq!(text_block["text"], "Rust is a systems programming language.");

    // Request blob should be in API format with messages array
    assert!(!steps[0].request_blob.is_empty());
    let req_bytes = s.blobs.get(&steps[0].request_blob).unwrap();
    let req: serde_json::Value = serde_json::from_slice(&req_bytes).unwrap();
    let messages = req["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "What is Rust?");

    // Token aggregation should have updated
    assert!(sess.total_tokens > 0);
}

// ── Test 2: Skips tool_use-only entries ──────────────────

#[tokio::test]
async fn test_sync_skips_tool_use_only() {
    let (state, store, _tmp, _hooks) = setup();
    let transcript_dir = TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");

    write_transcript(
        &transcript_path,
        &[
            r#"{"type":"user","message":{"content":"Read file"},"timestamp":"2026-04-11T10:00:00Z"}"#,
            r#"{"type":"assistant","uuid":"resp-tool-only","message":{"model":"claude-opus-4-6","usage":{"input_tokens":50,"output_tokens":20},"content":[{"type":"tool_use","id":"tu_1","name":"Read","input":{"path":"/foo"}}]},"timestamp":"2026-04-11T10:00:01Z"}"#,
        ],
    );

    let session_id = create_hook_session(
        &state,
        "claude-sess-skip",
        transcript_path.to_str().unwrap(),
    );

    let created = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created, 0, "Tool-use-only entries should not create LlmCall steps");

    let s = store.lock().unwrap();
    let timeline = s.get_root_timeline(&session_id).unwrap().unwrap();
    let steps = s.get_steps(&timeline.id).unwrap();
    assert_eq!(steps.len(), 0);
}

// ── Test 3: Mixed text + tool_use creates step ──────────

#[tokio::test]
async fn test_sync_creates_step_for_mixed_content() {
    let (state, store, _tmp, _hooks) = setup();
    let transcript_dir = TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");

    write_transcript(
        &transcript_path,
        &[
            r#"{"type":"user","message":{"content":"Read the config"},"timestamp":"2026-04-11T10:00:00Z"}"#,
            r#"{"type":"assistant","uuid":"resp-mixed","message":{"model":"claude-opus-4-6","usage":{"input_tokens":80,"output_tokens":30},"content":[{"type":"text","text":"Let me read that file for you."},{"type":"tool_use","id":"tu_1","name":"Read","input":{"path":"/config.toml"}}]},"timestamp":"2026-04-11T10:00:01Z"}"#,
        ],
    );

    let session_id = create_hook_session(
        &state,
        "claude-sess-mixed",
        transcript_path.to_str().unwrap(),
    );

    let created = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created, 1);

    let s = store.lock().unwrap();
    let timeline = s.get_root_timeline(&session_id).unwrap().unwrap();
    let steps = s.get_steps(&timeline.id).unwrap();
    assert_eq!(steps.len(), 1);

    let resp_bytes = s.blobs.get(&steps[0].response_blob).unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    let content = resp["content"].as_array().unwrap();
    let text_block = content.iter().find(|b| b["type"] == "text").unwrap();
    assert_eq!(text_block["text"], "Let me read that file for you.");
    assert_eq!(resp["has_tool_use"], true);
}

// ── Test 4: Idempotency — duplicate sync is a no-op ─────

#[tokio::test]
async fn test_sync_idempotent() {
    let (state, store, _tmp, _hooks) = setup();
    let transcript_dir = TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");

    write_transcript(
        &transcript_path,
        &[
            r#"{"type":"user","message":{"content":"Hi"},"timestamp":"2026-04-11T10:00:00Z"}"#,
            r#"{"type":"assistant","uuid":"resp-idem","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5},"content":[{"type":"text","text":"Hello!"}]},"timestamp":"2026-04-11T10:00:01Z"}"#,
        ],
    );

    let session_id = create_hook_session(
        &state,
        "claude-sess-idem",
        transcript_path.to_str().unwrap(),
    );

    // First sync: creates 1 step
    let created1 = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created1, 1);

    // Reset the byte offset to simulate a server restart
    {
        let s = store.lock().unwrap();
        let sess = s.get_session(&session_id).unwrap().unwrap();
        let mut meta = sess.metadata.clone();
        meta["transcript_byte_offset"] = serde_json::json!(0);
        s.update_session_metadata(&session_id, &meta).unwrap();
    }

    // Second sync: UUID dedup should prevent duplicates
    let created2 = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created2, 0, "Second sync should create 0 steps (dedup)");

    let s = store.lock().unwrap();
    let timeline = s.get_root_timeline(&session_id).unwrap().unwrap();
    let steps = s.get_steps(&timeline.id).unwrap();
    assert_eq!(steps.len(), 1, "Should still have exactly 1 step after dedup");
}

// ── Test 5: Incremental sync — only processes new lines ──

#[tokio::test]
async fn test_sync_incremental() {
    let (state, store, _tmp, _hooks) = setup();
    let transcript_dir = TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");

    // Start with 1 exchange
    write_transcript(
        &transcript_path,
        &[
            r#"{"type":"user","message":{"content":"Question 1"},"timestamp":"2026-04-11T10:00:00Z"}"#,
            r#"{"type":"assistant","uuid":"resp-inc-1","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5},"content":[{"type":"text","text":"Answer 1"}]},"timestamp":"2026-04-11T10:00:01Z"}"#,
        ],
    );

    let session_id = create_hook_session(
        &state,
        "claude-sess-inc",
        transcript_path.to_str().unwrap(),
    );

    // First sync
    let created1 = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created1, 1);

    // Append a second exchange
    append_transcript(
        &transcript_path,
        &[
            r#"{"type":"user","message":{"content":"Question 2"},"timestamp":"2026-04-11T10:01:00Z"}"#,
            r#"{"type":"assistant","uuid":"resp-inc-2","message":{"model":"claude-opus-4-6","usage":{"input_tokens":15,"output_tokens":8},"content":[{"type":"text","text":"Answer 2"}]},"timestamp":"2026-04-11T10:01:01Z"}"#,
        ],
    );

    // Second sync: should only process the new lines
    let created2 = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created2, 1, "Second sync should only create 1 new step");

    let s = store.lock().unwrap();
    let timeline = s.get_root_timeline(&session_id).unwrap().unwrap();
    let steps = s.get_steps(&timeline.id).unwrap();
    assert_eq!(steps.len(), 2, "Should have 2 total steps after 2 syncs");
}

// ── Test 6: Cursor format (role instead of type) ────────

#[tokio::test]
async fn test_sync_cursor_format() {
    let (state, store, _tmp, _hooks) = setup();
    let transcript_dir = TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");

    write_transcript(
        &transcript_path,
        &[
            r#"{"role":"user","message":{"content":"Hello from Cursor"}}"#,
            r#"{"role":"assistant","message":{"model":"claude-3.5-sonnet","usage":{"input_tokens":20,"output_tokens":10},"content":[{"type":"text","text":"Hi from Claude in Cursor!"}]}}"#,
        ],
    );

    let session_id = create_hook_session(
        &state,
        "claude-sess-cursor",
        transcript_path.to_str().unwrap(),
    );

    let created = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created, 1);

    let s = store.lock().unwrap();
    let timeline = s.get_root_timeline(&session_id).unwrap().unwrap();
    let steps = s.get_steps(&timeline.id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].model, "claude-3.5-sonnet");

    // Without UUID, dedup_id should be a content hash
    let tool_name = steps[0].tool_name.as_ref().unwrap();
    assert!(tool_name.starts_with("transcript:"));
    assert!(tool_name.len() > 15); // "transcript:" + hex hash
}

// ── Test 7: Thinking blocks included in preview ─────────

#[tokio::test]
async fn test_sync_thinking_preview() {
    let (state, store, _tmp, _hooks) = setup();
    let transcript_dir = TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");

    write_transcript(
        &transcript_path,
        &[
            r#"{"type":"user","message":{"content":"Think about this"},"timestamp":"2026-04-11T10:00:00Z"}"#,
            r#"{"type":"assistant","uuid":"resp-think","message":{"model":"claude-opus-4-6","usage":{"input_tokens":50,"output_tokens":30},"content":[{"type":"thinking","thinking":"Internal reasoning about the problem..."},{"type":"text","text":"Here is my conclusion."}]},"timestamp":"2026-04-11T10:00:01Z"}"#,
        ],
    );

    create_hook_session(
        &state,
        "claude-sess-think",
        transcript_path.to_str().unwrap(),
    );

    let created = rewind_web::transcript::sync_transcript_steps(&state).unwrap();
    assert_eq!(created, 1);

    let s = store.lock().unwrap();
    let sessions = s.list_sessions().unwrap();
    let timeline = s.get_root_timeline(&sessions[0].id).unwrap().unwrap();
    let steps = s.get_steps(&timeline.id).unwrap();

    let resp_bytes = s.blobs.get(&steps[0].response_blob).unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    let content = resp["content"].as_array().unwrap();
    let text_block = content.iter().find(|b| b["type"] == "text").unwrap();
    assert_eq!(text_block["text"], "Here is my conclusion.");
    let thinking_block = content.iter().find(|b| b["type"] == "thinking").unwrap();
    assert_eq!(thinking_block["thinking"], "Internal reasoning about the problem...");
}
