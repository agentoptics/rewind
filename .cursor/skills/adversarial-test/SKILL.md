---
name: adversarial-test
description: Run an exhaustive adversarial audit of Rewind, acting as a competitor trying to break every feature. Covers all 16 phases from build verification through security testing. Uses mock LLM (no API keys needed by default).
user-invocable: true
argument-hint: "[quick|full|real-api|site|phase N]"
---

# Rewind Adversarial Test

Exhaustive adversarial audit acting as a competitor trying every feature and edge case. Outputs a PASS/FAIL/WARN report with severity ratings (P0-P3).

## Arguments

- `full` (default) — Run all phases (0–15), no API keys needed
- `quick` — Build + mock-only phases (skip site and web UI manual checks)
- `real-api` — Include Phase 16 (real OpenAI API calls, requires `OPENAI_API_KEY`)
- `site` — Only Phase 15 (marketing site verification)
- `phase N` — Run a specific phase by number (0–16)

## What It Tests

| Phase | Tests | Requires |
|-------|-------|----------|
| 0 | Build: `cargo build --release`, `cargo clippy`, `cargo test`, `pytest`, `npm test`, version sync | Nothing |
| 1 | README quickstart: `rewind demo`, `show`, `sessions`, `web` | Built binary |
| 2 | Core recording: direct mode (OpenAI + Anthropic), streaming, async, concurrent | Mock LLM |
| 3 | Fork, replay, diff, savings report | Mock LLM |
| 4 | Assertions: baseline CRUD, self-check, duplicate names, special chars | Built binary |
| 5 | Evaluation: dataset CRUD, import/export JSONL, evaluator types, edge cases | Built binary |
| 6 | Multi-agent: nested spans, span tree API, thread grouping | Mock LLM |
| 7 | Web dashboard: all API endpoints, error handling, 404s, WebSocket | `rewind web` running |
| 8 | MCP server: all 26 tools via JSON-RPC stdin/stdout | Built binary |
| 9 | OTel: export dry-run, JSON import, Langfuse import error handling | Built binary |
| 10 | Session sharing: HTML generation, structure validation | Built binary |
| 11 | Snapshots: create, list, restore | Built binary |
| 12 | CLI edge cases: invalid inputs, SQL injection, read-only enforcement | Built binary |
| 13 | Python SDK: double init, decorators, async, exception handling, nesting | Python SDK |
| 14 | Security: malformed payloads, SQL injection, blob dedup, concurrency | `rewind web` running |
| 15 | Marketing site: build, pages, OG tags, RSS, broken links, drafts | `site/` directory |
| 16 | Real API: non-streaming, streaming, recording verification | `OPENAI_API_KEY` |
| 17 | rewind fix: diagnosis, --step, --hypothesis, --apply, --command, error handling | Built binary |

## Execution Instructions

### Phase 0: Build Verification

```bash
cd /Users/jain.r/workspace/rewind
cargo build --release 2>&1 | tail -5
cargo clippy --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20
cd python && pip install -e . && pytest tests/ -v 2>&1 | tail -30; cd ..
cd web && npm ci && npm test 2>&1 | tail -15; cd ..

# Version sync check
echo "Cargo: $(grep '^version' Cargo.toml | head -1)"
echo "Python: $(grep '__version__' python/rewind_agent/__init__.py)"
echo "CLI: $(grep 'CLI_VERSION' python/rewind_cli.py)"
```

### Phase 1: README Quickstart

```bash
# Clean DB if needed (stale DB causes P0 crash)
rm -f ~/.rewind/rewind.db ~/.rewind/rewind.db-shm ~/.rewind/rewind.db-wal
./target/release/rewind demo
./target/release/rewind show latest
./target/release/rewind sessions
```

### Phases 2–14: Automated Test Script

Start the web dashboard first (needed for Phases 7, 14):

```bash
# Terminal 1: Start web server
./target/release/rewind web --port 4800
```

Then run the automated adversarial test suite:

```bash
# Terminal 2: Run tests
python3 tests/adversarial_test.py          # Mock-only (default)
python3 tests/adversarial_test.py --real-api  # Include real API tests
```

The script runs Phases 2–14 (and optionally 16), uses the mock LLM server for zero-cost testing, and prints a structured PASS/FAIL/WARN report at the end. Exit code 1 if any test fails.

### Phase 15: Marketing Site

```bash
cd site && npm ci && npm run build 2>&1 | tail -10
python3 -c "
import re, os
html = open('dist/index.html').read()
checks = [('og:title', 'og:title' in html), ('og:image', 'og:image' in html),
          ('twitter:card', 'twitter:card' in html), ('install cmd', 'pip install' in html),
          ('nav', '<nav' in html), ('footer', '<footer' in html)]
for name, ok in checks:
    print(f'  {chr(0x2713) if ok else chr(0x2717)} {name}')

# Check for draft posts in sitemap
sitemap = open('dist/sitemap-0.xml').read()
for mdx in os.listdir('src/content/blog/'):
    if mdx.endswith('.mdx'):
        content = open(f'src/content/blog/{mdx}').read()
        if 'draft: true' in content:
            slug = mdx.replace('.mdx','')
            if slug in sitemap:
                print(f'  {chr(0x2717)} Draft post {slug} is in sitemap!')
            else:
                print(f'  {chr(0x2713)} Draft post {slug} correctly excluded')
"
```

### Phase 8: MCP Server (standalone)

```bash
python3 -c "
import subprocess, json, time
proc = subprocess.Popen(['./target/release/rewind-mcp'],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
def send(msg):
    proc.stdin.write(json.dumps(msg).encode() + b'\n')
    proc.stdin.flush()
def recv():
    line = proc.stdout.readline()
    return json.loads(line) if line else None

send({'jsonrpc':'2.0','id':1,'method':'initialize','params':{
    'protocolVersion':'2024-11-05','capabilities':{},
    'clientInfo':{'name':'test','version':'1.0'}}})
time.sleep(0.5)
init = recv()
send({'jsonrpc':'2.0','method':'notifications/initialized'})
time.sleep(0.2)
send({'jsonrpc':'2.0','id':2,'method':'tools/list','params':{}})
time.sleep(0.5)
tools = recv()
tool_names = [t['name'] for t in tools['result']['tools']]
print(f'Tools: {len(tool_names)}')

for name in ['list_sessions','cache_stats','list_baselines','list_eval_datasets','list_threads','list_snapshots']:
    send({'jsonrpc':'2.0','id':100,'method':'tools/call','params':{'name':name,'arguments':{}}})
    time.sleep(0.3)
    r = recv()
    ok = r and 'result' in r and not r['result'].get('isError')
    print(f'  {chr(0x2713) if ok else chr(0x2717)} {name}')
proc.terminate()
"
```

## Summary Report Format

The test script outputs:

```
======================================================================
  ADVERSARIAL TEST REPORT -- Rewind
======================================================================

  Total: 65  |  PASS: 55  |  FAIL: 5  |  WARN: 5  |  SKIP: 0
  Pass rate: 84.6%

  FAILURES:
    [P1] [P5] Create dataset ...
    ...

  WARNINGS:
    [P2] [P4] Special chars in name ...
    ...
======================================================================
```

Severity ratings:
- **P0** — Product broken, ship-blocker
- **P1** — Feature broken, needs fix before next release
- **P2** — UX issue, can ship but should fix
- **P3** — Cosmetic / informational

## Known Issues to Watch For

These were found in the v0.10.0 audit and may or may not be fixed:

1. **P0** — `rewind demo` crashes with "disk I/O error" on stale/corrupted DB from prior version
2. **P1** — Python SDK `_patch_existing_clients` crashes when `openai._client` is None (`patch.py:368`)
3. **P1** — `rewind restore latest` doesn't support "latest" alias
4. **P1** — Savings API ignores session ID prefix (needs full UUID)
5. **P1** — `rewind share --include-content` hangs in non-TTY (no `--yes` flag)
6. **P1** — OTel export Web API returns 501, ignores request body endpoint
7. **P2** — Draft blog post included in production sitemap
