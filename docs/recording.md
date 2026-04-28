# Recording

**Rewind** is a time-travel debugger for AI agents. It records every LLM call your agent makes — the full request, response, context window, token counts, and timing — so you can inspect, fork, replay, and diff later.

This page covers the recording modes, agent hooks for enriching traces, and streaming behavior.

---

## Five ways to record + replay

Choose the approach that fits your stack:

| | **Direct mode** (Python) | **HTTP intercept** (Python) | **`cached_llm_call`** (Python) | **Proxy mode** (any language) | **Dashboard runner** (Python, Phase 3) |
|:---|:---|:---|:---|:---|:---|
| **Setup** | `rewind_agent.init()` — one line | `rewind_agent.intercept.install()` — one line | `@cached_llm_call(...)` decorator | `rewind record` in a second terminal | Register a runner; click "Run replay" in the dashboard |
| **Languages** | Python (OpenAI + Anthropic SDKs) | Python (any HTTP-based LLM client) | Python (any sync/async function returning a dict-like response) | Any language that makes HTTP calls | Python (long-lived runner process) |
| **How it works** | Monkey-patches SDK clients in-process | Patches HTTP transport layer (httpx, requests, aiohttp) | Wraps a single function with cache-then-live + record | HTTP proxy intercepts LLM traffic | Dashboard POSTs HMAC-signed webhook to the runner; runner runs the agent under `intercept.install()` and posts progress events back |
| **Custom gateways** | No (only OpenAI/Anthropic SDKs) | **Yes** (custom predicate matches any host) | N/A — caller decides what to wrap | Yes (point upstream at anything) | Inherits from intercept (any predicate the runner installs) |
| **Streaming** | Captured via stream wrappers | Pass-through on miss; synthetic SSE on hit | Pass-through (function returns whatever the underlying call returns) | SSE pass-through, zero added latency | Inherits from intercept |
| **Best for** | Quick iteration with OpenAI/Anthropic SDK | Custom HTTP clients, mTLS gateways, third-party LLM wrappers | Per-function granular caching when intercept is too broad | Polyglot teams, non-Python agents | Operator-driven replays from the dashboard against registered agent processes |

**Picking between modes:**

- **Use `init()`** if you call OpenAI / Anthropic SDKs directly and want zero-config.
- **Use `intercept.install()`** if your agent talks to LLMs through a custom HTTP client, gateway, or proxy. See the [HTTP Intercept Quickstart](intercept-quickstart.md).
- **Use `@cached_llm_call`** for one-off functions where transport-level intercept is too broad. See [`cached-llm-call.md`](cached-llm-call.md).
- **Use proxy mode** for non-Python stacks or polyglot teams.
- **Use the dashboard runner** when you want a human in the loop pressing "Run replay" against a long-lived agent process. See [`runners.md`](runners.md).

---

## Direct mode

No proxy, no second terminal, no environment variables. Add one line and every LLM call is recorded:

```python
import rewind_agent

rewind_agent.init()  # patches OpenAI + Anthropic automatically

# Or as a scoped session:
with rewind_agent.session("my-agent"):
    client = openai.OpenAI()
    client.chat.completions.create(model="gpt-4o", messages=[...])
```

Under the hood, `init()` monkey-patches the OpenAI and Anthropic Python SDK clients so that every call is captured and written to `~/.rewind/`. No configuration beyond that single line.

```python
import rewind_agent
import openai

rewind_agent.init()  # that's it — all LLM calls are now recorded

client = openai.OpenAI()
client.chat.completions.create(model="gpt-4o", messages=[...])
# Recorded to ~/.rewind/ — inspect with: rewind show latest
```

---

## Proxy mode

Works with any language or framework. Start the proxy, point your agent at it:

```bash
rewind record --name "my-agent" --upstream https://api.openai.com
# In another terminal:
export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
python3 my_agent.py   # or node, go, rust — anything that calls the LLM
```

The proxy intercepts every HTTP request to the LLM API, records it, and forwards it upstream. Streaming (SSE) responses are passed through in real-time — the agent sees zero added latency while Rewind accumulates the full response for storage.

---

## Agent hooks — enrich recordings with semantic labels

Without hooks, the recording shows "LLM Call 1", "LLM Call 2". With hooks, steps show up as "search", "plan", "execute" — much more useful when debugging.

```python
import rewind_agent

@rewind_agent.step("search")
def search_web(query: str) -> str:
    return client.chat.completions.create(...)

@rewind_agent.tool("calculator")
def calculate(a: float, b: float) -> float:
    return a + b

@rewind_agent.node("planner")       # LangGraph-style node
def plan(state: dict) -> dict:
    return {"steps": ["research", "write", "review"]}

with rewind_agent.trace("analysis_phase"):
    rewind_agent.annotate("confidence", 0.92)
    result = search_web("Tokyo population")
```

### Hook reference

| Decorator / Function | Purpose |
|:---|:---|
| `@step("name")` | Label a function as a named step in the trace |
| `@tool("name")` | Label a function as a tool invocation |
| `@node("name")` | Label a function as a graph node (LangGraph-style) |
| `trace("name")` | Context manager that groups nested calls under a named span |
| `annotate(key, value)` | Attach arbitrary metadata to the current step |

---

## Streaming

Both recording modes handle streaming transparently:

- **Proxy mode**: SSE streams are forwarded to the agent in real-time while being accumulated for recording. The agent sees zero added latency.
- **Direct mode**: Stream wrappers capture chunks as they arrive, then write the full assembled response to storage after the stream completes.

---

## Examples

See these example scripts for working code:

- [`examples/01_basic_recording.py`](../examples/01_basic_recording.py) — Minimal proxy-mode recording
- [`examples/03_python_hooks.py`](../examples/03_python_hooks.py) — `@step`, `@tool`, `@node`, `trace()`, and `annotate()`
- [`examples/05_direct_mode.py`](../examples/05_direct_mode.py) — Direct mode with `init()` and `session()`
