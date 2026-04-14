"""
AI-powered diagnosis for failed Rewind sessions.

Uses function calling to produce structured diagnosis output: root cause,
failed step, fix type, and fix parameters.

Can be used:
  1. As a subprocess: `python3 -m rewind_agent.fix` (called by Rust CLI)
  2. In-process via `run_diagnosis()` (called by Python SDK)

Provider scope (v1): OpenAI SDK format only — works with OpenAI, Ollama, vLLM,
LiteLLM, and any OpenAI-compatible endpoint.
"""

import json
import os
import sys
import time


# ── Fix Type Definitions ────────────────────────────────────────

VALID_FIX_TYPES = ["swap_model", "inject_system", "adjust_temperature", "retry_step", "no_fix"]


# ── Diagnosis Prompt ────────────────────────────────────────────

_DIAGNOSIS_TEMPLATE = """\
You are an expert AI agent debugger. Analyze this failed agent session and diagnose the root cause.

## Session Overview
Name: {session_name}
Status: {session_status}
Total steps: {total_steps}

## Step Sequence
{step_summary}

## Failure Point — Step {failure_step_number}
Type: {failure_step_type}
Model: {failure_model}
Tokens in: {failure_tokens_in}, Tokens out: {failure_tokens_out}
Duration: {failure_duration_ms}ms
Tool: {failure_tool_name}
Error: {failure_error}

### Request (what the agent sent to the LLM):
{failure_request}

### Response (what the LLM returned):
{failure_response}

## Preceding Context
{preceding_context}

{expected_section}

## Instructions
Diagnose the root cause and recommend a fix. Consider:
- Context window overflow (tokens approaching model limits)
- Model capability mismatch (using a weaker model for a hard task)
- Missing or unclear instructions (system prompt gaps)
- Non-deterministic failure (might succeed on retry)
- Agent code issue (not fixable via LLM parameter changes)
"""


# ── Client Construction (copied from llm_judge.py) ─────────────

def _get_openai_client(config: dict):
    """Lazy-import openai and build a client from env-based config."""
    try:
        import openai
    except ImportError:
        raise RuntimeError(
            "rewind fix requires the openai package. "
            "Install with: pip install rewind-agent[openai]"
        )

    api_key_env = config.get("api_key_env", "OPENAI_API_KEY")
    api_key = os.environ.get(api_key_env)
    if not api_key:
        raise RuntimeError(
            f"rewind fix requires an API key. "
            f"Set {api_key_env} in your environment."
        )

    api_base_env = config.get("api_base_env", "OPENAI_BASE_URL")
    base_url = os.environ.get(api_base_env)

    return openai.OpenAI(api_key=api_key, base_url=base_url)


# ── Retry Logic (copied from llm_judge.py) ──────────────────────

def _is_retryable(error: Exception) -> bool:
    try:
        import openai
        if isinstance(error, openai.RateLimitError):
            return True
        if isinstance(error, openai.APIStatusError) and error.status_code in (500, 502, 503):
            return True
    except ImportError:
        pass
    if isinstance(error, (ConnectionError, TimeoutError)):
        return True
    return False


# ── Helpers ─────────────────────────────────────────────────────

def _to_str(val, max_len: int = 0) -> str:
    if val is None:
        return "(empty)"
    if isinstance(val, str):
        s = val
    else:
        s = json.dumps(val, indent=2, default=str)
    if max_len and len(s) > max_len:
        return s[:max_len] + f"\n... (truncated, {len(s)} chars total)"
    return s


def _build_step_summary(steps: list) -> str:
    lines = []
    for s in steps:
        status_mark = "✓" if s.get("status") == "success" else "✗" if s.get("status") == "error" else "?"
        tool_info = f" [{s['tool_name']}]" if s.get("tool_name") else ""
        lines.append(
            f"  {status_mark} Step {s['step_number']}: {s['step_type']}{tool_info} "
            f"({s.get('model', '?')}, {s.get('tokens_in', 0)}+{s.get('tokens_out', 0)} tok, "
            f"{s.get('duration_ms', 0)}ms)"
            + (f" — ERROR: {s['error']}" if s.get("error") else "")
        )
    return "\n".join(lines)


def _build_preceding_context(preceding: list, max_blob_len: int = 15000) -> str:
    if not preceding:
        return "(no preceding steps provided)"
    parts = []
    for p in preceding:
        parts.append(f"### Step {p['step_number']}")
        parts.append(f"Request:\n{_to_str(p.get('request'), max_blob_len)}")
        parts.append(f"Response:\n{_to_str(p.get('response'), max_blob_len)}")
        parts.append("")
    return "\n".join(parts)


# ── Core Diagnosis Logic ────────────────────────────────────────

def run_diagnosis(payload: dict) -> dict:
    """
    Run an LLM-powered diagnosis on a failed session.

    Returns: {root_cause, failed_step, fork_from, fix_type, fix_params, explanation, confidence}
    """
    session = payload.get("session", {})
    steps = payload.get("steps", [])
    failure_step_num = payload.get("failure_step")
    failure_ctx = payload.get("failure_context", {})
    expected = payload.get("expected")
    config = payload.get("config", {})

    model = config.get("model", "gpt-4o-mini")
    temperature = config.get("temperature", 0)

    failure_step = None
    for s in steps:
        if s["step_number"] == failure_step_num:
            failure_step = s
            break
    if not failure_step:
        failure_step = steps[-1] if steps else {}

    expected_section = ""
    if expected:
        expected_section = f"## Expected Behavior\nThe user expected: {expected}\n"

    prompt = _DIAGNOSIS_TEMPLATE.format(
        session_name=session.get("name", "unknown"),
        session_status=session.get("status", "unknown"),
        total_steps=session.get("total_steps", len(steps)),
        step_summary=_build_step_summary(steps),
        failure_step_number=failure_step.get("step_number", "?"),
        failure_step_type=failure_step.get("step_type", "?"),
        failure_model=failure_step.get("model", "?"),
        failure_tokens_in=failure_step.get("tokens_in", 0),
        failure_tokens_out=failure_step.get("tokens_out", 0),
        failure_duration_ms=failure_step.get("duration_ms", 0),
        failure_tool_name=failure_step.get("tool_name") or "(none)",
        failure_error=failure_step.get("error") or "(no error message)",
        failure_request=_to_str(failure_ctx.get("request"), 30000),
        failure_response=_to_str(failure_ctx.get("response"), 30000),
        preceding_context=_build_preceding_context(
            failure_ctx.get("preceding_steps", [])
        ),
        expected_section=expected_section,
    )

    tools = [{
        "type": "function",
        "function": {
            "name": "diagnose_failure",
            "description": "Provide a structured diagnosis of the agent failure.",
            "parameters": {
                "type": "object",
                "properties": {
                    "root_cause": {
                        "type": "string",
                        "description": "Clear explanation of why the agent failed.",
                    },
                    "failed_step": {
                        "type": "integer",
                        "description": "The step number where the failure occurred.",
                    },
                    "fork_from": {
                        "type": "integer",
                        "description": "The step number to fork from (one before the failure).",
                    },
                    "fix_type": {
                        "type": "string",
                        "enum": VALID_FIX_TYPES,
                        "description": "The category of fix to apply.",
                    },
                    "fix_params": {
                        "type": "object",
                        "description": "Parameters for the fix (e.g., {\"model\": \"gpt-4o\"} for swap_model).",
                    },
                    "explanation": {
                        "type": "string",
                        "description": "Why this fix should resolve the issue.",
                    },
                    "confidence": {
                        "type": "string",
                        "enum": ["high", "medium", "low"],
                        "description": "Confidence level: high (clear error + known fix), medium (error identified but fix speculative), low (inferring from behavior).",
                    },
                },
                "required": ["root_cause", "failed_step", "fork_from", "fix_type", "fix_params", "explanation", "confidence"],
            },
        },
    }]

    client_config = {
        k: v for k, v in config.items()
        if k in ("api_key_env", "api_base_env")
    }
    client = _get_openai_client(client_config)

    last_error = None
    for attempt in range(3):
        try:
            response = client.chat.completions.create(
                model=model,
                temperature=temperature,
                messages=[{"role": "user", "content": prompt}],
                tools=tools,
                tool_choice={"type": "function", "function": {"name": "diagnose_failure"}},
            )

            message = response.choices[0].message
            if message.tool_calls and len(message.tool_calls) > 0:
                call = message.tool_calls[0]
                args = json.loads(call.function.arguments)

                fix_type = args.get("fix_type", "no_fix")
                if fix_type not in VALID_FIX_TYPES:
                    fix_type = "no_fix"

                return {
                    "root_cause": args.get("root_cause", "Unknown"),
                    "failed_step": args.get("failed_step", failure_step_num),
                    "fork_from": args.get("fork_from", max(1, (failure_step_num or 1) - 1)),
                    "fix_type": fix_type,
                    "fix_params": args.get("fix_params", {}),
                    "explanation": args.get("explanation", ""),
                    "confidence": args.get("confidence", "low"),
                }

            content = (message.content or "").strip()
            return {
                "root_cause": f"Diagnosis LLM did not use function calling. Raw: {content[:500]}",
                "failed_step": failure_step_num,
                "fork_from": max(1, (failure_step_num or 1) - 1),
                "fix_type": "no_fix",
                "fix_params": {},
                "explanation": "",
                "confidence": "low",
            }

        except Exception as e:
            last_error = e
            if _is_retryable(e):
                wait = 2 ** attempt
                time.sleep(wait)
                continue
            break

    return {
        "root_cause": f"Diagnosis failed: {last_error}",
        "failed_step": failure_step_num,
        "fork_from": max(1, (failure_step_num or 1) - 1),
        "fix_type": "no_fix",
        "fix_params": {},
        "explanation": "",
        "confidence": "low",
    }


# ── Subprocess Entry Point ──────────────────────────────────────

def main():
    """Entry point for `python3 -m rewind_agent.fix`."""
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as e:
        result = {
            "root_cause": f"Invalid stdin JSON: {e}",
            "failed_step": None, "fork_from": None,
            "fix_type": "no_fix", "fix_params": {},
            "explanation": "", "confidence": "low",
        }
        print(json.dumps(result))
        sys.exit(1)

    try:
        result = run_diagnosis(payload)
    except (ValueError, RuntimeError) as e:
        result = {
            "root_cause": f"Diagnosis config error: {e}",
            "failed_step": None, "fork_from": None,
            "fix_type": "no_fix", "fix_params": {},
            "explanation": "", "confidence": "low",
        }
        print(json.dumps(result))
        sys.exit(1)
    except Exception as e:
        result = {
            "root_cause": f"Diagnosis error: {e}",
            "failed_step": None, "fork_from": None,
            "fix_type": "no_fix", "fix_params": {},
            "explanation": "", "confidence": "low",
        }

    print(json.dumps(result))


if __name__ == "__main__":
    main()
