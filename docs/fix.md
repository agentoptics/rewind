# `rewind fix` — AI-Powered Diagnosis and Repair

`rewind fix` analyzes a failed agent session, diagnoses the root cause with an LLM, suggests a fix, and optionally forks + replays with the fix applied to verify it works. One command from "broken" to "proven fix."

## Quickstart

```bash
# Diagnose the latest session
rewind fix latest

# Apply the suggested fix (fork + replay with patch)
rewind fix latest --apply

# Fully automated: diagnose, fork, replay, report
rewind fix latest --apply --yes --command "python agent.py"
```

## How It Works

```
rewind fix <session>
  │
  ├─ 1. Load session + steps + request/response blobs from local SQLite
  ├─ 2. Find the failure step (first error, or --step N)
  ├─ 3. Call your LLM to diagnose (via OPENAI_API_KEY, same as rewind eval score)
  ├─ 4. LLM returns: root cause, fix type, fix params, confidence
  │
  ├─ [if --apply]
  │   ├─ 5. Fork the timeline at the suggested step
  │   ├─ 6. Start proxy with request rewrites (model swap, system inject, etc.)
  │   ├─ 7. Run agent against the patched proxy (or wait for manual re-run)
  │   └─ 8. Print replay savings (tokens, cost, time saved)
  │
  └─ Print diagnosis + next steps
```

## Modes

### Diagnosis only (default)

```bash
rewind fix latest
```

Prints the root cause, suggested fix type, and confidence level. No changes made — read-only analysis.

```
⏪ Diagnosing session "research-agent-demo" (5 steps)...

  Failure: Step 5 — llmcall (gpt-4o) — error
  Error: HALLUCINATION: Agent used stale 2019 projection as current fact
  Root cause: The agent relied on outdated data due to a search API rate
              limit, leading to incorrect population figures.

  Suggested fix: retry_step
  Reasoning: Retrying may allow the agent to fetch updated data.
  Confidence: high

  To apply this fix automatically:
    rewind fix latest --apply
```

### Apply (interactive)

```bash
rewind fix latest --apply
```

After diagnosis, shows a dry-run preview and asks for confirmation. Then forks the timeline, starts the proxy with request rewrites, and waits for you to re-run your agent against the proxy.

### Apply with command (automated)

```bash
rewind fix latest --apply --yes --command "python agent.py"
```

Fully automated: diagnoses, forks, starts the proxy, sets `OPENAI_BASE_URL` and `ANTHROPIC_BASE_URL` to point at the proxy, runs your agent command, and prints replay savings when done.

### Hypothesis (skip diagnosis)

```bash
rewind fix latest --hypothesis "swap_model:gpt-4o" --apply --yes
```

Skips the LLM diagnosis call entirely. Directly tests your theory by forking and replaying with the specified fix applied. Useful when you already know what to try.

## Fix Types

| Fix Type | Syntax | What the Proxy Does |
|:---|:---|:---|
| `swap_model` | `--hypothesis swap_model:gpt-4o` | Rewrites the `model` field in LLM requests (same provider only) |
| `inject_system` | `--hypothesis "inject_system:Be more careful"` | Prepends/appends a system message (OpenAI + Anthropic formats) |
| `adjust_temperature` | `--hypothesis adjust_temperature:0.2` | Overrides the `temperature` parameter |
| `retry_step` | `--hypothesis retry_step` | Replays with no changes — exploits LLM non-determinism |
| `no_fix` | *(diagnosis only)* | Issue is in agent code, not LLM behavior |

## Flags

| Flag | Description |
|:---|:---|
| `<session>` | Session ID, prefix, or `latest` (required) |
| `--diagnosis-model <model>` | Model for the diagnosis LLM call (default: `gpt-4o-mini`) |
| `--apply` | Fork and replay with the suggested fix |
| `--command <cmd>` / `-c <cmd>` | Agent command to run against the patched proxy (requires `--apply`) |
| `--hypothesis <fix>` | Skip diagnosis, test a fix directly (requires `--apply`) |
| `--step <N>` | Analyze a specific step instead of auto-detecting |
| `--expected <desc>` | Describe expected behavior (for soft failures with no error step) |
| `--upstream <url>` | Upstream LLM base URL (default: `https://api.openai.com`) |
| `--port <port>` | Proxy port for replay (default: `8443`) |
| `--yes` | Skip confirmation prompts |
| `--json` | Output diagnosis as machine-readable JSON |

## JSON Output

```bash
rewind fix latest --json
```

```json
{
  "root_cause": "The agent relied on outdated data...",
  "failed_step": 5,
  "fork_from": 4,
  "fix_type": "retry_step",
  "fix_params": {},
  "explanation": "Retrying may allow the agent to fetch updated data.",
  "confidence": "high"
}
```

## Requirements

- **`OPENAI_API_KEY`** — required for the diagnosis LLM call. Uses your own API key (same as `rewind eval score`). The diagnosis call uses `gpt-4o-mini` by default (~$0.03 per diagnosis).
- **Proxy-recorded sessions** — `--apply` requires sessions recorded via `rewind record` (proxy mode). Diagnosis works on all session types (proxy, direct, hooks, OTel import).
- **`rewind-agent[openai]`** — the diagnosis subprocess requires the OpenAI Python package. Install with `pip install rewind-agent[openai]`.

## Failure Step Detection

When no `--step` is specified, `rewind fix` auto-detects the failure:

1. First step with `status: error`
2. Last step if the session status is `failed`
3. Last step (with a warning suggesting `--expected`)

For soft failures (every step returns 200 OK but the output is wrong), use `--expected`:

```bash
rewind fix latest --expected "Should report Tokyo population as 13.96M"
```

## Examples

### Diagnose and manually apply

```bash
# See what went wrong
rewind fix latest

# Apply the suggestion
rewind fix latest --apply

# After replay, compare the timelines
rewind diff latest main fix-retry_step
rewind eval score latest --compare-timelines -e task_completion
```

### Test your own theory

```bash
# "I think switching to gpt-4o will fix the context window issue"
rewind fix latest --hypothesis "swap_model:gpt-4o" --apply --yes --command "python agent.py"

# "Maybe adding a system prompt will prevent hallucination"
rewind fix latest --hypothesis "inject_system:Only use data from the search results. Do not infer." --apply --yes --command "python agent.py"

# "Maybe it'll just work on retry"
rewind fix latest --hypothesis retry_step --apply --yes --command "python agent.py"
```
