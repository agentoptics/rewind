"""
Adversarial test suite for Rewind.
Run: python3 tests/adversarial_test.py [--real-api] [--web-port PORT]

Phases 2-6, 9-14: Core recording, fork/replay/diff, assertions, evaluation,
multi-agent, OTel, sharing, snapshots, CLI edge cases, SDK edge cases, security.

Uses mock LLM server -- zero API tokens consumed (unless --real-api).
Requires: cargo build --release && pip install -e python/
"""

import sys
import os
import json
import time
import subprocess
import tempfile
import shutil
import argparse

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "e2e"))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))

REWIND = os.path.join(os.path.dirname(__file__), "..", "target", "release", "rewind")
RESULTS = []
WEB_PORT = 4800


def result(phase, test, status, detail=""):
    severity = "P0" if status == "FAIL" and "crash" in detail.lower() else \
               "P1" if status == "FAIL" else \
               "P2" if status == "WARN" else "P3"
    RESULTS.append({"phase": phase, "test": test, "status": status, "severity": severity, "detail": detail})
    icon = {"PASS": "\u2713", "FAIL": "\u2717", "WARN": "\u26a0", "SKIP": "\u25cb"}[status]
    print(f"  {icon} [{phase}] {test}: {status} {detail}")


def run(cmd, timeout=30):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, shell=isinstance(cmd, str))
        return r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired:
        return -1, "", "TIMEOUT"
    except Exception as e:
        return -1, "", str(e)


def api_url(path):
    return f"http://127.0.0.1:{WEB_PORT}{path}"


# ═══════════════════════════════════════════════════════════════
# Phase 2: Core Recording with Mock LLM
# ═══════════════════════════════════════════════════════════════
def phase2_recording():
    print("\n\u2550\u2550 Phase 2: Core Recording (Mock LLM) \u2550\u2550")

    import mock_llm_server
    mock_url = mock_llm_server.start(port=9876)

    try:
        import openai
        import rewind_agent

        rewind_agent.init()
        client = openai.OpenAI(api_key="mock-key", base_url=f"{mock_url}/v1")
        resp = client.chat.completions.create(
            model="gpt-4o-mini",
            messages=[{"role": "user", "content": "What is the capital of France?"}],
            stream=False,
        )
        rewind_agent.uninit()

        if resp.choices[0].message.content == "Paris":
            result("P2", "Direct mode OpenAI non-streaming", "PASS")
        else:
            result("P2", "Direct mode OpenAI non-streaming", "FAIL", f"Got: {resp.choices[0].message.content}")
    except Exception as e:
        result("P2", "Direct mode OpenAI non-streaming", "FAIL", f"Crash: {e}")

    code, out, err = run([REWIND, "sessions"])
    if code == 0:
        result("P2", "Session recorded after direct mode", "PASS")
    else:
        result("P2", "Session recorded after direct mode", "FAIL", err)

    code, out, err = run([REWIND, "show", "latest"])
    if code == 0 and "gpt-4o-mini" in out:
        result("P2", "Show latest has model name", "PASS")
    else:
        result("P2", "Show latest has model name", "FAIL", f"Output: {out[:200]}")

    try:
        import openai
        import rewind_agent
        rewind_agent.init(session_name="stream-test")
        client = openai.OpenAI(api_key="mock-key", base_url=f"{mock_url}/v1")
        stream = client.chat.completions.create(
            model="gpt-4o-mini",
            messages=[{"role": "user", "content": "Write a haiku about the ocean"}],
            stream=True,
        )
        chunks = []
        for chunk in stream:
            if chunk.choices and chunk.choices[0].delta.content:
                chunks.append(chunk.choices[0].delta.content)
        rewind_agent.uninit()

        full_text = "".join(chunks)
        if len(full_text) > 5:
            result("P2", "Direct mode OpenAI streaming", "PASS")
        else:
            result("P2", "Direct mode OpenAI streaming", "WARN", f"Short content: {full_text[:100]}")
    except Exception as e:
        result("P2", "Direct mode OpenAI streaming", "FAIL", f"Crash: {e}")

    try:
        import anthropic
        import rewind_agent
        rewind_agent.init(session_name="anthropic-test")
        client = anthropic.Anthropic(api_key="mock-key", base_url=mock_url)
        resp = client.messages.create(
            model="claude-3-5-sonnet-latest",
            max_tokens=100,
            messages=[{"role": "user", "content": "What is 2+2?"}],
        )
        rewind_agent.uninit()
        text = resp.content[0].text if resp.content else ""
        if "4" in text:
            result("P2", "Direct mode Anthropic non-streaming", "PASS")
        else:
            result("P2", "Direct mode Anthropic non-streaming", "FAIL", f"Got: {text}")
    except ImportError:
        result("P2", "Direct mode Anthropic non-streaming", "SKIP", "anthropic not installed")
    except Exception as e:
        result("P2", "Direct mode Anthropic non-streaming", "FAIL", f"Crash: {e}")

    try:
        import asyncio
        import openai
        import rewind_agent
        rewind_agent.init(session_name="async-test")
        client = openai.AsyncOpenAI(api_key="mock-key", base_url=f"{mock_url}/v1")

        async def do_async():
            return await client.chat.completions.create(
                model="gpt-4o-mini",
                messages=[{"role": "user", "content": "hello"}],
            )

        resp = asyncio.run(do_async())
        rewind_agent.uninit()
        if resp.choices[0].message.content:
            result("P2", "Async recording", "PASS")
        else:
            result("P2", "Async recording", "FAIL", "Empty response")
    except Exception as e:
        result("P2", "Async recording", "FAIL", f"Crash: {e}")

    try:
        import asyncio
        import openai
        import rewind_agent
        rewind_agent.init(session_name="concurrent-async")
        client = openai.AsyncOpenAI(api_key="mock-key", base_url=f"{mock_url}/v1")

        async def multi_async():
            tasks = [
                client.chat.completions.create(
                    model="gpt-4o-mini",
                    messages=[{"role": "user", "content": f"What is {q}?"}]
                )
                for q in ["hello", "2+2", "color"]
            ]
            return await asyncio.gather(*tasks)

        results_async = asyncio.run(multi_async())
        rewind_agent.uninit()
        if len(results_async) == 3:
            result("P2", "Concurrent async calls", "PASS")
        else:
            result("P2", "Concurrent async calls", "FAIL", f"Got {len(results_async)} results")
    except Exception as e:
        result("P2", "Concurrent async calls", "FAIL", f"Crash: {e}")

    mock_llm_server.stop()


# ═══════════════════════════════════════════════════════════════
# Phase 3: Fork, Replay, Diff
# ═══════════════════════════════════════════════════════════════
def phase3_fork_replay():
    print("\n\u2550\u2550 Phase 3: Fork, Replay, Diff \u2550\u2550")

    import mock_llm_server
    mock_url = mock_llm_server.start(port=9877)

    try:
        import openai
        import rewind_agent
        rewind_agent.init(session_name="fork-test")
        client = openai.OpenAI(api_key="mock-key", base_url=f"{mock_url}/v1")
        for q in ["coral reef", "reversible", "summarize"]:
            client.chat.completions.create(
                model="gpt-4o-mini",
                messages=[{"role": "user", "content": f"Tell me about {q}"}],
            )
        rewind_agent.uninit()
        result("P3", "Record multi-step session", "PASS")
    except Exception as e:
        result("P3", "Record multi-step session", "FAIL", f"Crash: {e}")
        mock_llm_server.stop()
        return

    code, out, err = run([REWIND, "sessions"])
    lines = [l for l in out.split("\n") if "fork-test" in l]
    if not lines:
        result("P3", "Find fork-test session", "FAIL", "Not found")
        mock_llm_server.stop()
        return
    session_id = lines[0].split()[1] if len(lines[0].split()) > 1 else ""
    result("P3", "Find fork-test session", "PASS")

    code, out, err = run([REWIND, "fork", session_id, "--at", "2", "--label", "fix-step2"])
    result("P3", "Fork at step 2", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "diff", session_id, "main", "fix-step2"])
    result("P3", "Diff main vs fork", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "diff", session_id, "main", "nonexistent-timeline"])
    result("P3", "Diff nonexistent timeline errors", "PASS" if code != 0 else "WARN", "No error" if code == 0 else "")

    mock_llm_server.stop()


# ═══════════════════════════════════════════════════════════════
# Phase 4: Assertions
# ═══════════════════════════════════════════════════════════════
def phase4_assertions():
    print("\n\u2550\u2550 Phase 4: Assertions \u2550\u2550")

    code, out, err = run([REWIND, "assert", "baseline", "latest", "--name", "adv-test-baseline"])
    result("P4", "Create baseline", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "assert", "list"])
    result("P4", "List baselines", "PASS" if code == 0 and "adv-test-baseline" in out else "FAIL", out[:100])

    code, out, err = run([REWIND, "assert", "show", "adv-test-baseline"])
    result("P4", "Show baseline detail", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "assert", "check", "latest", "--against", "adv-test-baseline"])
    result("P4", "Self-check passes", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "assert", "baseline", "latest", "--name", "adv-test-baseline"])
    result("P4", "Duplicate baseline rejected", "PASS" if code != 0 else "WARN", "Silently overwrote" if code == 0 else "")

    code, out, err = run([REWIND, "assert", "baseline", "latest", "--name", "test baseline/special"])
    result("P4", "Special chars in name", "WARN" if code == 0 else "PASS",
           "Accepted spaces/slashes" if code == 0 else "Rejected properly")

    code, out, err = run([REWIND, "assert", "delete", "adv-test-baseline"])
    result("P4", "Delete baseline", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "assert", "delete", "nonexistent-baseline"])
    result("P4", "Delete nonexistent errors", "PASS" if code != 0 else "WARN", "No error" if code == 0 else "")


# ═══════════════════════════════════════════════════════════════
# Phase 5: Evaluation System (uses positional args, not --name)
# ═══════════════════════════════════════════════════════════════
def phase5_evaluation():
    print("\n\u2550\u2550 Phase 5: Evaluation System \u2550\u2550")

    code, out, err = run([REWIND, "eval", "dataset", "create", "adv-test-ds", "-d", "Adversarial test"])
    result("P5", "Create dataset", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "eval", "dataset", "list"])
    result("P5", "List datasets", "PASS" if code == 0 and "adv-test-ds" in out else "FAIL", out[:100])

    code, out, err = run([REWIND, "eval", "dataset", "show", "adv-test-ds"])
    result("P5", "Show empty dataset", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "eval", "dataset", "create", "adv-test-ds", "-d", "duplicate"])
    result("P5", "Duplicate dataset rejected", "PASS" if code != 0 else "WARN", "Allowed duplicate" if code == 0 else "")

    with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
        f.write(json.dumps({"input": {"query": "What is 2+2?"}, "expected": {"answer": "4"}}) + "\n")
        f.write(json.dumps({"input": {"query": "Capital of France?"}, "expected": {"answer": "Paris"}}) + "\n")
        jsonl_path = f.name

    code, out, err = run([REWIND, "eval", "dataset", "import", "adv-import-ds", jsonl_path])
    result("P5", "Import from JSONL", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    with tempfile.NamedTemporaryFile(suffix='.jsonl', delete=False) as f:
        export_path = f.name
    code, out, err = run([REWIND, "eval", "dataset", "export", "adv-import-ds", "--output", export_path])
    result("P5", "Export to JSONL", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
        f.write("this is not json\n")
        bad_jsonl = f.name
    code, out, err = run([REWIND, "eval", "dataset", "import", "bad-jsonl-ds", bad_jsonl])
    result("P5", "Malformed JSONL rejected", "PASS" if code != 0 else "WARN", "Accepted malformed" if code == 0 else "")

    with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
        f.write("")
        empty_jsonl = f.name
    code, out, err = run([REWIND, "eval", "dataset", "import", "empty-ds", empty_jsonl])
    result("P5", "Empty JSONL rejected", "PASS" if code != 0 else "WARN", "Accepted empty" if code == 0 else "")

    code, out, err = run([REWIND, "eval", "evaluator", "create", "adv-exact", "-t", "exact_match"])
    result("P5", "Create exact_match evaluator", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "eval", "evaluator", "create", "adv-contains", "-t", "contains"])
    result("P5", "Create contains evaluator", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "eval", "evaluator", "create", "adv-regex", "-t", "regex", "-c", '{"pattern": "\\\\d+"}'])
    result("P5", "Create regex evaluator", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "eval", "evaluator", "list"])
    result("P5", "List evaluators", "PASS" if code == 0 and "adv-exact" in out else "FAIL", out[:100])

    code, out, err = run([REWIND, "eval", "dataset", "delete", "adv-test-ds"])
    result("P5", "Delete dataset", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    for p in [jsonl_path, export_path, bad_jsonl, empty_jsonl]:
        try: os.unlink(p)
        except: pass


# ═══════════════════════════════════════════════════════════════
# Phase 6: Multi-Agent Tracing
# ═══════════════════════════════════════════════════════════════
def phase6_multiagent():
    print("\n\u2550\u2550 Phase 6: Multi-Agent Tracing \u2550\u2550")

    import mock_llm_server
    mock_url = mock_llm_server.start(port=9878)

    try:
        import openai
        import rewind_agent
        rewind_agent.init(session_name="multiagent-test")
        client = openai.OpenAI(api_key="mock-key", base_url=f"{mock_url}/v1")

        with rewind_agent.span("supervisor"):
            with rewind_agent.span("researcher"):
                client.chat.completions.create(
                    model="gpt-4o-mini",
                    messages=[{"role": "user", "content": "Search for quantum computing"}],
                )
            with rewind_agent.span("writer"):
                client.chat.completions.create(
                    model="gpt-4o-mini",
                    messages=[{"role": "user", "content": "Summarize the findings"}],
                )
        rewind_agent.uninit()
        result("P6", "Nested span recording", "PASS")
    except Exception as e:
        result("P6", "Nested span recording", "FAIL", f"Crash: {e}")

    code, out, err = run([REWIND, "show", "latest"])
    result("P6", "Span tree in show output", "PASS" if code == 0 and ("supervisor" in out or "researcher" in out) else "WARN",
           "" if "supervisor" in out else f"Spans not visible: {out[:100]}")

    try:
        import rewind_agent
        rewind_agent.init(session_name="thread-test-1")
        with rewind_agent.thread("conversation-1"):
            client.chat.completions.create(
                model="gpt-4o-mini",
                messages=[{"role": "user", "content": "hello"}],
            )
        rewind_agent.uninit()
        code, out, err = run([REWIND, "threads"])
        result("P6", "Thread listing", "PASS" if code == 0 else "FAIL", err if code != 0 else "")
    except Exception as e:
        result("P6", "Thread listing", "FAIL", f"Crash: {e}")

    mock_llm_server.stop()


# ═══════════════════════════════════════════════════════════════
# Phase 9: OTel Export/Import
# ═══════════════════════════════════════════════════════════════
def phase9_otel():
    print("\n\u2550\u2550 Phase 9: OTel Export/Import \u2550\u2550")

    code, out, err = run([REWIND, "export", "otel", "latest", "--dry-run"])
    result("P9", "OTel export --dry-run", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    otlp_json = {
        "resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "test"}}]},
            "scopeSpans": [{"scope": {"name": "test"}, "spans": [{
                "traceId": "0123456789abcdef0123456789abcdef", "spanId": "0123456789abcdef",
                "name": "test-span", "kind": 1,
                "startTimeUnixNano": str(int(time.time() * 1e9)),
                "endTimeUnixNano": str(int((time.time() + 1) * 1e9)),
                "attributes": [], "status": {}
            }]}]}]
    }
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
        json.dump(otlp_json, f)
        json_path = f.name

    code, out, err = run([REWIND, "import", "otel", "--json-file", json_path, "--name", "otel-import-test"])
    result("P9", "OTel import from JSON", "PASS" if code == 0 else "FAIL", err if code != 0 else "")
    os.unlink(json_path)

    code, out, err = run([REWIND, "import", "from-langfuse", "--trace", "fake-trace-id"])
    result("P9", "Langfuse import without creds errors", "PASS" if code != 0 else "FAIL",
           "Should error" if code == 0 else "")


# ═══════════════════════════════════════════════════════════════
# Phase 10: Session Sharing
# ═══════════════════════════════════════════════════════════════
def phase10_sharing():
    print("\n\u2550\u2550 Phase 10: Session Sharing \u2550\u2550")
    with tempfile.NamedTemporaryFile(suffix='.html', delete=False) as f:
        share_path = f.name

    code, out, err = run([REWIND, "share", "latest", "--output", share_path])
    if code == 0 and os.path.exists(share_path) and os.path.getsize(share_path) > 100:
        result("P10", "Share generates HTML", "PASS", f"Size: {os.path.getsize(share_path)} bytes")
    else:
        result("P10", "Share generates HTML", "FAIL", err if code != 0 else "File too small")

    try:
        with open(share_path) as f:
            html = f.read()
        result("P10", "HTML has proper structure", "PASS" if "<html" in html.lower() and "rewind" in html.lower() else "FAIL")
        os.unlink(share_path)
    except Exception as e:
        result("P10", "HTML has proper structure", "FAIL", str(e))


# ═══════════════════════════════════════════════════════════════
# Phase 11: Snapshots
# ═══════════════════════════════════════════════════════════════
def phase11_snapshots():
    print("\n\u2550\u2550 Phase 11: Snapshots \u2550\u2550")
    test_dir = tempfile.mkdtemp(prefix="rewind-snap-test-")
    with open(os.path.join(test_dir, "test.txt"), "w") as f:
        f.write("original content")

    code, out, err = run([REWIND, "snapshot", test_dir, "--label", "adv-snap-test"])
    result("P11", "Create snapshot", "PASS" if code == 0 else "FAIL", err if code != 0 else "")

    code, out, err = run([REWIND, "snapshots"])
    result("P11", "List snapshots", "PASS" if code == 0 and "adv-snap-test" in out else "FAIL", out[:100])

    shutil.rmtree(test_dir, ignore_errors=True)


# ═══════════════════════════════════════════════════════════════
# Phase 12: CLI Edge Cases
# ═══════════════════════════════════════════════════════════════
def phase12_cli_edge():
    print("\n\u2550\u2550 Phase 12: CLI Edge Cases \u2550\u2550")

    code, _, _ = run([REWIND, "show", "nonexistent-session-id"])
    result("P12", "Show nonexistent session errors", "PASS" if code != 0 else "FAIL")

    code, _, _ = run([REWIND, "replay", "latest", "--from", "99999"])
    result("P12", "Replay out-of-range step errors", "PASS" if code != 0 else "WARN")

    code, _, _ = run([REWIND, "fork", "latest", "--at", "0"])
    result("P12", "Fork at step 0", "WARN" if code == 0 else "PASS", "Accepted step 0" if code == 0 else "")

    code, out, err = run([REWIND, "query", "DROP TABLE sessions"])
    code2, out2, _ = run([REWIND, "sessions"])
    result("P12", "SQL injection: DROP TABLE blocked", "PASS" if code2 == 0 else "FAIL",
           "CRITICAL DATA LOSS" if code2 != 0 else "Read-only enforced")

    code, out, err = run([REWIND, "query", "'; DROP TABLE sessions; --"])
    code2, out2, _ = run([REWIND, "sessions"])
    result("P12", "SQL injection: semicolon blocked", "PASS" if code2 == 0 else "FAIL",
           "CRITICAL DATA LOSS" if code2 != 0 else "")

    code, _, _ = run([REWIND, "query", "SELECT count(*) FROM sessions"])
    result("P12", "Valid read-only query works", "PASS" if code == 0 else "FAIL")

    code, out, _ = run([REWIND, "query", "--tables"])
    result("P12", "Query --tables lists schema", "PASS" if code == 0 and "sessions" in out else "FAIL")

    code, _, _ = run([REWIND, "diff", "nonexistent", "main", "fork"])
    result("P12", "Diff nonexistent session errors", "PASS" if code != 0 else "WARN")


# ═══════════════════════════════════════════════════════════════
# Phase 13: Python SDK Edge Cases
# ═══════════════════════════════════════════════════════════════
def phase13_sdk_edge():
    print("\n\u2550\u2550 Phase 13: Python SDK Edge Cases \u2550\u2550")
    import rewind_agent

    try:
        rewind_agent.init(session_name="double-init-1")
        rewind_agent.init(session_name="double-init-2")
        rewind_agent.uninit()
        result("P13", "Double init() doesn't crash", "PASS")
    except Exception as e:
        result("P13", "Double init() doesn't crash", "FAIL", f"Crash: {e}")

    try:
        rewind_agent.uninit()
        result("P13", "uninit() without init()", "PASS")
    except Exception as e:
        result("P13", "uninit() without init()", "FAIL", f"Crash: {e}")

    try:
        @rewind_agent.step("test-step")
        def my_step():
            return 42
        rewind_agent.init(session_name="decorator-test")
        r = my_step()
        rewind_agent.uninit()
        result("P13", "@step on function", "PASS" if r == 42 else "FAIL", f"Return changed: {r}" if r != 42 else "")
    except Exception as e:
        result("P13", "@step on function", "FAIL", f"Crash: {e}")

    try:
        import asyncio
        @rewind_agent.step("async-step")
        async def my_async_step():
            return 43
        rewind_agent.init(session_name="async-dec-test")
        r = asyncio.run(my_async_step())
        rewind_agent.uninit()
        result("P13", "@step on async function", "PASS" if r == 43 else "FAIL")
    except Exception as e:
        result("P13", "@step on async function", "FAIL", f"Crash: {e}")

    try:
        @rewind_agent.tool("failing-tool")
        def bad_tool():
            raise ValueError("intentional error")
        rewind_agent.init(session_name="error-tool-test")
        try: bad_tool()
        except ValueError: pass
        rewind_agent.uninit()
        result("P13", "@tool with exception", "PASS")
    except Exception as e:
        result("P13", "@tool with exception", "PASS" if "intentional" in str(e) else "FAIL", str(e)[:80])

    try:
        rewind_agent.init(session_name="deep-nesting")
        with rewind_agent.span("l1"):
            with rewind_agent.span("l2"):
                with rewind_agent.span("l3"):
                    with rewind_agent.span("l4"):
                        pass
        rewind_agent.uninit()
        result("P13", "4-level deep span nesting", "PASS")
    except Exception as e:
        result("P13", "4-level deep span nesting", "FAIL", f"Crash: {e}")

    try:
        rewind_agent.wrap_langgraph(None)
        result("P13", "wrap_langgraph(None)", "WARN", "No error for None input")
    except (TypeError, AttributeError, ValueError):
        result("P13", "wrap_langgraph(None)", "PASS")
    except Exception as e:
        result("P13", "wrap_langgraph(None)", "FAIL", str(e)[:80])


# ═══════════════════════════════════════════════════════════════
# Phase 14: Security
# ═══════════════════════════════════════════════════════════════
def phase14_security():
    print("\n\u2550\u2550 Phase 14: Security \u2550\u2550")
    import urllib.request, urllib.error

    try:
        req = urllib.request.Request(api_url("/api/hooks/event"),
            data=b"this is not json", headers={"Content-Type": "application/json"}, method="POST")
        try: status = urllib.request.urlopen(req).status
        except urllib.error.HTTPError as e: status = e.code
        result("P14", "Malformed JSON returns error", "PASS" if status >= 400 else "FAIL", f"Status: {status}")
    except Exception as e:
        result("P14", "Malformed JSON returns error", "FAIL", str(e)[:80])

    try:
        req = urllib.request.Request(api_url("/v1/traces"),
            data=b"\x00\x01\x02garbage", headers={"Content-Type": "application/x-protobuf"}, method="POST")
        try: status = urllib.request.urlopen(req).status
        except urllib.error.HTTPError as e: status = e.code
        result("P14", "Malformed protobuf returns error", "PASS" if status >= 400 else "WARN", f"Status: {status}")
    except Exception as e:
        result("P14", "Malformed protobuf returns error", "FAIL", str(e)[:80])

    try:
        import mock_llm_server, openai, rewind_agent
        mock_url = mock_llm_server.start(port=9879)
        rewind_agent.init(session_name="'; DROP TABLE sessions; --")
        client = openai.OpenAI(api_key="mock-key", base_url=f"{mock_url}/v1")
        client.chat.completions.create(model="gpt-4o-mini", messages=[{"role": "user", "content": "hello"}])
        rewind_agent.uninit()
        code, _, _ = run([REWIND, "sessions"])
        result("P14", "SQL injection in session name", "PASS" if code == 0 else "FAIL")
        mock_llm_server.stop()
    except Exception as e:
        result("P14", "SQL injection in session name", "FAIL", f"Crash: {e}")

    try:
        from rewind_agent.store import BlobStore
        blob_dir = tempfile.mkdtemp()
        bs = BlobStore(blob_dir)
        h1, h2 = bs.put(b"identical content"), bs.put(b"identical content")
        result("P14", "Blob store deduplication", "PASS" if h1 == h2 else "FAIL")
        shutil.rmtree(blob_dir, ignore_errors=True)
    except Exception as e:
        result("P14", "Blob store deduplication", "FAIL", str(e)[:80])

    try:
        import concurrent.futures
        def hit_api(_):
            return urllib.request.urlopen(api_url("/api/sessions")).status
        with concurrent.futures.ThreadPoolExecutor(max_workers=20) as pool:
            statuses = [f.result(timeout=10) for f in [pool.submit(hit_api, i) for i in range(50)]]
        result("P14", "50 concurrent API requests", "PASS" if all(s == 200 for s in statuses) else "WARN")
    except Exception as e:
        result("P14", "50 concurrent API requests", "FAIL", str(e)[:80])


# ═══════════════════════════════════════════════════════════════
# Phase 17: rewind fix (AI Diagnosis + Proxy Rewriting)
# ═══════════════════════════════════════════════════════════════
def phase17_fix():
    print("\n\u2550\u2550 Phase 17: rewind fix \u2550\u2550")

    # Ensure demo session exists for fix tests
    run([REWIND, "demo"])

    # ── Diagnosis-only (no API key needed — tests error path gracefully) ──

    code, out, err = run([REWIND, "fix", "latest"])
    result("P17", "Fix latest (no API key)", "PASS" if code == 0 else "FAIL",
           "Should degrade gracefully" if code != 0 else "")

    code, out, err = run([REWIND, "fix", "latest", "--json"])
    if code == 0:
        try:
            parsed = json.loads(out)
            has_fields = all(k in parsed for k in ["fix_type", "root_cause", "confidence"])
            result("P17", "Fix --json output is valid JSON", "PASS" if has_fields else "FAIL",
                   f"Missing fields" if not has_fields else "")
        except json.JSONDecodeError:
            result("P17", "Fix --json output is valid JSON", "FAIL", f"Invalid JSON: {out[:100]}")
    else:
        result("P17", "Fix --json output is valid JSON", "FAIL", err[:100])

    # ── --step N: valid step ──
    code, out, err = run([REWIND, "fix", "latest", "--step", "3", "--json"])
    result("P17", "Fix --step 3 (valid)", "PASS" if code == 0 else "FAIL", err[:100] if code != 0 else "")

    # ── --step N: invalid step ──
    code, out, err = run([REWIND, "fix", "latest", "--step", "999"])
    result("P17", "Fix --step 999 (invalid) errors", "PASS" if code != 0 and "not found" in (out + err).lower() else "FAIL",
           "Should error with step range" if code == 0 else "")

    # ── --hypothesis without --apply ──
    code, out, err = run([REWIND, "fix", "latest", "--hypothesis", "swap_model:gpt-4o"])
    result("P17", "Hypothesis without --apply rejected", "PASS" if code != 0 else "FAIL",
           "Should require --apply" if code == 0 else "")

    # ── --command without --apply (clap rejects) ──
    code, out, err = run([REWIND, "fix", "latest", "--command", "echo hi"])
    result("P17", "Command without --apply rejected", "PASS" if code != 0 else "FAIL",
           "clap should reject" if code == 0 else "")

    # ── --hypothesis: valid fix types ──
    for fix_type, param in [("swap_model:gpt-4o", True), ("inject_system:Be careful", True),
                             ("adjust_temperature:0.2", True), ("retry_step", True)]:
        code, out, err = run([REWIND, "fix", "latest", "--hypothesis", fix_type, "--apply", "--yes",
                              "--command", "echo test", "--port", "19876"])
        # These will fork + start proxy + run echo, should succeed
        result("P17", f"Hypothesis {fix_type.split(':')[0]}", "PASS" if code == 0 else "FAIL",
               err[:100] if code != 0 else "")

    # ── --hypothesis: invalid fix type ──
    code, out, err = run([REWIND, "fix", "latest", "--hypothesis", "bad_type:foo", "--apply"])
    result("P17", "Invalid hypothesis type rejected", "PASS" if code != 0 and "unknown" in (out + err).lower() else "FAIL",
           "Should list valid types" if code == 0 else "")

    # ── --hypothesis: swap_model without param ──
    code, out, err = run([REWIND, "fix", "latest", "--hypothesis", "swap_model", "--apply"])
    result("P17", "swap_model without param rejected", "PASS" if code != 0 else "FAIL",
           "Should require model name" if code == 0 else "")

    # ── --hypothesis: adjust_temperature with non-number ──
    code, out, err = run([REWIND, "fix", "latest", "--hypothesis", "adjust_temperature:abc", "--apply"])
    result("P17", "adjust_temperature non-number rejected", "PASS" if code != 0 else "FAIL",
           "Should require number" if code == 0 else "")

    # ── --apply blocked for hooks session ──
    code, out, err = run([REWIND, "query", "SELECT id FROM sessions WHERE source = 'hooks' LIMIT 1"])
    if code == 0 and out.strip():
        lines = [l.strip() for l in out.strip().split("\n") if l.strip() and not l.strip().startswith("id") and "─" not in l]
        if lines:
            hooks_id = lines[0].split()[0]
            code2, out2, err2 = run([REWIND, "fix", hooks_id, "--hypothesis", "retry_step", "--apply"])
            result("P17", "Apply blocked for hooks session", "PASS" if code2 != 0 and "proxy" in (out2 + err2).lower() else "FAIL",
                   "Should require proxy session" if code2 == 0 else "")
        else:
            result("P17", "Apply blocked for hooks session", "SKIP", "No hooks session found")
    else:
        result("P17", "Apply blocked for hooks session", "SKIP", "No hooks session found")

    # ── nonexistent session ──
    code, out, err = run([REWIND, "fix", "nonexistent-session"])
    result("P17", "Fix nonexistent session errors", "PASS" if code != 0 else "FAIL")

    # ── empty session (no steps) ── 
    # Create a session with zero steps via query
    code, out, err = run([REWIND, "fix", "latest", "--step", "0"])
    result("P17", "Fix --step 0 errors", "PASS" if code != 0 else "WARN",
           "Step 0 should not exist" if code == 0 else "")

    # ── Dry-run preview: cancel with 'n' ──
    try:
        proc = subprocess.Popen(
            [REWIND, "fix", "latest", "--hypothesis", "retry_step", "--apply"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        stdout, stderr = proc.communicate(input="n\n", timeout=10)
        combined = stdout + stderr
        result("P17", "Dry-run cancel with 'n'", "PASS" if "cancelled" in combined.lower() and proc.returncode == 0 else "FAIL",
               f"exit={proc.returncode}" if proc.returncode != 0 else "")
    except subprocess.TimeoutExpired:
        proc.kill()
        result("P17", "Dry-run cancel with 'n'", "FAIL", "Timed out waiting for prompt")
    except Exception as e:
        result("P17", "Dry-run cancel with 'n'", "FAIL", str(e)[:80])

    # ── Verify fork created by fix is diffable ──
    code, out, err = run([REWIND, "fix", "latest", "--hypothesis", "retry_step", "--apply", "--yes",
                          "--command", "echo done", "--port", "19877"])
    if code == 0:
        # Find the latest fork
        code2, out2, _ = run([REWIND, "query",
                              "SELECT t.id FROM timelines t WHERE t.label LIKE 'fix-%' ORDER BY t.created_at DESC LIMIT 1"])
        if code2 == 0:
            fork_lines = [l.strip() for l in out2.strip().split("\n") if l.strip() and not l.strip().startswith("id") and "─" not in l]
            if fork_lines:
                fork_id = fork_lines[0].split()[0]
                code3, _, err3 = run([REWIND, "diff", "latest", "main", fork_id[:8]])
                result("P17", "Fix fork is diffable", "PASS" if code3 == 0 else "FAIL", err3[:80] if code3 != 0 else "")
            else:
                result("P17", "Fix fork is diffable", "WARN", "Could not find fork timeline")
        else:
            result("P17", "Fix fork is diffable", "WARN", "Query failed")
    else:
        result("P17", "Fix fork is diffable", "FAIL", f"Apply failed: {err[:80]}")


# ═══════════════════════════════════════════════════════════════
# Phase 16: Real API (optional, requires OPENAI_API_KEY)
# ═══════════════════════════════════════════════════════════════
def phase16_real_api():
    print("\n\u2550\u2550 Phase 16: Real API Integration \u2550\u2550")

    if not os.environ.get("OPENAI_API_KEY"):
        result("P16", "All real API tests", "SKIP", "OPENAI_API_KEY not set")
        return

    import openai, rewind_agent
    total_tokens = 0

    try:
        rewind_agent.init(session_name="real-api-nonstream")
        client = openai.OpenAI()
        resp = client.chat.completions.create(model="gpt-4o-mini",
            messages=[{"role": "user", "content": "What is 2+2? Reply with just the number."}], max_tokens=5)
        total_tokens += resp.usage.total_tokens
        rewind_agent.uninit()
        result("P16", "Direct mode non-streaming", "PASS", f"{resp.usage.total_tokens} tokens")
    except Exception as e:
        result("P16", "Direct mode non-streaming", "FAIL", str(e)[:80])

    try:
        rewind_agent.init(session_name="real-api-stream")
        client = openai.OpenAI()
        stream = client.chat.completions.create(model="gpt-4o-mini",
            messages=[{"role": "user", "content": "Name one color. One word."}],
            max_tokens=5, stream=True, stream_options={"include_usage": True})
        chunks = []
        for chunk in stream:
            if chunk.choices and chunk.choices[0].delta.content:
                chunks.append(chunk.choices[0].delta.content)
            if chunk.usage:
                total_tokens += chunk.usage.total_tokens
        rewind_agent.uninit()
        result("P16", "Direct mode streaming", "PASS", f"'{(''.join(chunks)).strip()}'")
    except Exception as e:
        result("P16", "Direct mode streaming", "FAIL", str(e)[:80])

    code, out, err = run([REWIND, "show", "latest"])
    result("P16", "Real API call recorded", "PASS" if code == 0 and "gpt-4o-mini" in out else "FAIL")

    print(f"  Total API tokens consumed: ~{total_tokens}")


# ═══════════════════════════════════════════════════════════════
# Report
# ═══════════════════════════════════════════════════════════════
def print_report():
    print("\n" + "=" * 70)
    print("  ADVERSARIAL TEST REPORT -- Rewind")
    print("=" * 70)

    by_status = {}
    for r in RESULTS:
        by_status.setdefault(r["status"], []).append(r)

    total = len(RESULTS)
    passed = len(by_status.get("PASS", []))
    failed = len(by_status.get("FAIL", []))
    warned = len(by_status.get("WARN", []))
    skipped = len(by_status.get("SKIP", []))

    print(f"\n  Total: {total}  |  PASS: {passed}  |  FAIL: {failed}  |  WARN: {warned}  |  SKIP: {skipped}")
    if total:
        print(f"  Pass rate: {passed/total*100:.1f}%")

    if failed > 0:
        print(f"\n  {'~'*60}")
        print("  FAILURES:")
        for r in by_status.get("FAIL", []):
            print(f"    [{r['severity']}] [{r['phase']}] {r['test']}")
            if r["detail"]:
                print(f"         {r['detail'][:120]}")

    if warned > 0:
        print(f"\n  {'~'*60}")
        print("  WARNINGS:")
        for r in by_status.get("WARN", []):
            print(f"    [{r['severity']}] [{r['phase']}] {r['test']}")
            if r["detail"]:
                print(f"         {r['detail'][:120]}")

    print("=" * 70)

    if failed > 0:
        sys.exit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Rewind adversarial test suite")
    parser.add_argument("--real-api", action="store_true", help="Run real API tests (requires OPENAI_API_KEY)")
    parser.add_argument("--web-port", type=int, default=4800, help="Web dashboard port (default: 4800)")
    parser.add_argument("--phase", type=int, help="Run a specific phase only (e.g., --phase 17)")
    args = parser.parse_args()
    WEB_PORT = args.web_port

    phases = {
        2: phase2_recording,
        3: phase3_fork_replay,
        4: phase4_assertions,
        5: phase5_evaluation,
        6: phase6_multiagent,
        9: phase9_otel,
        10: phase10_sharing,
        11: phase11_snapshots,
        12: phase12_cli_edge,
        13: phase13_sdk_edge,
        14: phase14_security,
        16: phase16_real_api,
        17: phase17_fix,
    }

    if args.phase:
        if args.phase not in phases:
            print(f"Unknown phase {args.phase}. Available: {sorted(phases.keys())}")
            sys.exit(1)
        phases[args.phase]()
    else:
        for num, fn in sorted(phases.items()):
            if num == 16 and not args.real_api:
                continue
            fn()
    print_report()
