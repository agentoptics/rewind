# Changelog

## v0.1.0 (2026-04-09)

### Initial Release

**Core**
- **Recording proxy** — Local HTTP proxy intercepts all LLM API calls transparently. Streaming SSE pass-through for OpenAI and Anthropic. Zero code changes needed.
- **Interactive TUI** — Terminal UI with step-by-step timeline, context window viewer, and step details.
- **Timeline forking** — Branch execution at any step. Forked timelines share parent steps via structural sharing.
- **Timeline diffing** — Compare two timelines to see where they diverge.
- **Content-addressed storage** — SQLite + SHA-256 blob store (like git objects).

**Instant Replay**
- Proxy-level response caching by request hash. Identical requests served from cache at $0 cost, 0ms latency. Enable with `rewind record --replay`.

**Snapshots**
- Workspace checkpoint and restore without git. `rewind snapshot` captures a directory as compressed tar. `rewind restore` rolls back to any snapshot.

**Python SDK**
- `rewind_agent.init()` auto-patches OpenAI/Anthropic clients.
- `@step`, `@node`, `@tool` decorators and `trace()` context manager for enriching recordings.
- `wrap_langgraph()` and `wrap_crew()` for one-line framework integration.

**Compatibility**
- OpenAI, Anthropic, AWS Bedrock (via gateway), and any OpenAI-compatible API.
- Works with LangGraph, CrewAI, OpenAI Agents SDK, or custom code.
