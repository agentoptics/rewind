"""Tests for rewind_agent.runner — the operator-facing runner library
that processes dispatch webhooks."""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from rewind_agent import runner


# ──────────────────────────────────────────────────────────────────
# ProgressReporter
# ──────────────────────────────────────────────────────────────────


def test_progress_reporter_builds_url_from_base_url() -> None:
    reporter = runner.ProgressReporter(
        "job-x", base_url="http://dispatcher-supplied.example"
    )
    assert reporter._url == "http://dispatcher-supplied.example/api/replay-jobs/job-x/events"


def test_progress_reporter_strips_trailing_slash() -> None:
    reporter = runner.ProgressReporter(
        "job-x", base_url="http://example.com/"
    )
    assert reporter._url == "http://example.com/api/replay-jobs/job-x/events"


# ──────────────────────────────────────────────────────────────────
# DispatchPayload
# ──────────────────────────────────────────────────────────────────


def test_dispatch_payload_decodes_canonical_body() -> None:
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.job_id == "j"
    assert payload.session_id == "s"
    assert payload.replay_context_id == "r"
    assert payload.replay_context_timeline_id == "tl-fork"
    assert payload.base_url == "http://x.example"


def test_dispatch_payload_tolerates_missing_timeline_id_for_back_compat() -> None:
    """Older dispatchers (pre-#154) don't include
    replay_context_timeline_id; runner library decodes them anyway
    with an empty string. attach_replay_context will then leave
    _timeline_id unset (logged as a soft warning in user code)."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.replay_context_timeline_id == ""


def test_dispatch_payload_decodes_at_step() -> None:
    """v0.14.8+ servers include `at_step` in the dispatch body — the
    fork-point of the replay-context's timeline. Runners use it to
    drive multi-turn replay (start the agent at the right turn so
    edits to user messages in turn 2+ take effect)."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "at_step": 4,
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.at_step == 4


def test_dispatch_payload_at_step_defaults_to_none_for_back_compat() -> None:
    """Older servers (pre v0.14.8) don't send `at_step`. The SDK
    decodes the body anyway; runners that need at_step branch on
    `payload.at_step is not None` and fall back to single-turn
    behavior when it's missing."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.at_step is None


def test_dispatch_payload_tolerates_extra_unknown_keys() -> None:
    """Forward-compat: future server versions may add fields. Older
    runner SDKs hitting newer servers must keep working — extra
    keys in the body are ignored, not rejected."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "at_step": 4,
        "base_url": "http://x.example",
        "future_field_added_in_v0_14_99": "ignored",
        "another_future_field": {"nested": [1, 2, 3]},
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.job_id == "j"
    assert payload.at_step == 4


# ──────────────────────────────────────────────────────────────────
# asgi_handler — end-to-end with a mocked event endpoint
# ──────────────────────────────────────────────────────────────────


def test_asgi_handler_invalid_dispatch_body_returns_400() -> None:
    @runner.handle_replay
    async def handler(p, r) -> None:
        pass

    async def run():
        status, resp = await runner.asgi_handler(
            body_bytes=b'{"missing_required_fields": true}',
            handler=handler,
        )
        assert status == 400
        assert "error" in resp

    asyncio.run(run())


def test_asgi_handler_dispatches_user_code_on_valid_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    body = json.dumps(
        {"job_id": "j", "session_id": "s", "replay_context_id": "r", "base_url": "http://test"}
    ).encode()

    received_events: list[dict[str, Any]] = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        received_events.append(body)

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    handler_called_with: list[runner.DispatchPayload] = []

    @runner.handle_replay
    async def handler(payload: runner.DispatchPayload, reporter: runner.ProgressReporter) -> None:
        handler_called_with.append(payload)
        await reporter.progress(1, progress_total=3)
        await reporter.progress(2)
        await reporter.completed()

    async def run():
        status, resp = await runner.asgi_handler(
            body_bytes=body,
            handler=handler,
        )
        assert status == 202
        assert resp == {"job_id": "j", "accepted": True}

        for _ in range(30):
            if received_events and received_events[-1].get("event_type") == "completed":
                break
            await asyncio.sleep(0.05)
        else:
            raise AssertionError(f"handler did not finish; events={received_events}")

    asyncio.run(run())

    assert len(handler_called_with) == 1
    assert handler_called_with[0].job_id == "j"
    types = [e["event_type"] for e in received_events]
    assert types == ["started", "progress", "progress", "completed"]
    assert received_events[1]["step_number"] == 1
    assert received_events[1]["progress_total"] == 3


def test_asgi_handler_emits_errored_when_user_code_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    body = json.dumps(
        {"job_id": "j", "session_id": "s", "replay_context_id": "r", "base_url": "http://test"}
    ).encode()

    received_events: list[dict[str, Any]] = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        received_events.append(body)

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    @runner.handle_replay
    async def handler(payload, reporter) -> None:
        raise RuntimeError("agent fell over")

    async def run():
        status, _ = await runner.asgi_handler(
            body_bytes=body,
            handler=handler,
        )
        assert status == 202

        for _ in range(30):
            if any(e.get("event_type") == "errored" for e in received_events):
                break
            await asyncio.sleep(0.05)
        else:
            raise AssertionError(f"errored event never emitted; got: {received_events}")

    asyncio.run(run())

    err = next(e for e in received_events if e["event_type"] == "errored")
    assert "agent fell over" in err["error_message"]
    assert err["error_stage"] == "agent"


def test_asgi_handler_uses_payload_base_url_for_reporter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The reporter built by asgi_handler should prefer the dispatch
    payload's base_url over the default."""
    body = json.dumps(
        {
            "job_id": "j",
            "session_id": "s",
            "replay_context_id": "r",
            "base_url": "http://from-dispatch.example",
        }
    ).encode()

    captured_reporter: list[runner.ProgressReporter] = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        pass

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    @runner.handle_replay
    async def handler(payload, reporter) -> None:
        captured_reporter.append(reporter)

    async def run():
        await runner.asgi_handler(body_bytes=body, handler=handler)
        await asyncio.sleep(0.05)

    asyncio.run(run())

    assert len(captured_reporter) == 1
    assert "from-dispatch.example" in captured_reporter[0]._url


# ──────────────────────────────────────────────────────────────────
# attach_replay_context (env-var bootstrap)
# ──────────────────────────────────────────────────────────────────


def test_attach_replay_context_sets_contextvars() -> None:
    from rewind_agent.explicit import (
        ExplicitClient,
        _replay_context_id,
        _session_id,
    )

    client = ExplicitClient(base_url="http://127.0.0.1:4800")
    client.attach_replay_context(
        session_id="sess-attach", replay_context_id="ctx-attach"
    )
    assert _session_id.get() == "sess-attach"
    assert _replay_context_id.get() == "ctx-attach"


def test_install_bootstraps_from_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """``intercept.install()`` reads REWIND_SESSION_ID +
    REWIND_REPLAY_CONTEXT_ID and attaches before patching.
    """
    from rewind_agent.explicit import _replay_context_id, _session_id, _timeline_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "boot-sess")
    monkeypatch.setenv("REWIND_REPLAY_CONTEXT_ID", "boot-ctx")
    monkeypatch.delenv("REWIND_REPLAY_CONTEXT_TIMELINE_ID", raising=False)
    monkeypatch.setenv("REWIND_URL", "http://127.0.0.1:4800")

    _install._INSTALLED = False
    _session_id.set(None)
    _replay_context_id.set(None)
    _timeline_id.set(None)

    try:
        _install._bootstrap_replay_context_from_env()
        assert _session_id.get() == "boot-sess"
        assert _replay_context_id.get() == "boot-ctx"
        assert _timeline_id.get() is None
    finally:
        _session_id.set(None)
        _replay_context_id.set(None)
        _timeline_id.set(None)


def test_install_bootstraps_with_timeline_id_from_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Review #154 round 2: env-var bootstrap also honors
    REWIND_REPLAY_CONTEXT_TIMELINE_ID. Subprocess-bootstrap paths
    that previously left _timeline_id unset now propagate the fork
    timeline so live cache misses record into the right place."""
    from rewind_agent.explicit import _replay_context_id, _session_id, _timeline_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "boot-sess")
    monkeypatch.setenv("REWIND_REPLAY_CONTEXT_ID", "boot-ctx")
    monkeypatch.setenv("REWIND_REPLAY_CONTEXT_TIMELINE_ID", "boot-fork-tl")
    monkeypatch.setenv("REWIND_URL", "http://127.0.0.1:4800")

    _install._INSTALLED = False
    _session_id.set(None)
    _replay_context_id.set(None)
    _timeline_id.set(None)

    try:
        _install._bootstrap_replay_context_from_env()
        assert _session_id.get() == "boot-sess"
        assert _replay_context_id.get() == "boot-ctx"
        assert _timeline_id.get() == "boot-fork-tl"
    finally:
        _session_id.set(None)
        _replay_context_id.set(None)
        _timeline_id.set(None)


def test_install_partial_env_logs_warning_and_skips(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    from rewind_agent.explicit import _replay_context_id, _session_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "only-session")
    monkeypatch.delenv("REWIND_REPLAY_CONTEXT_ID", raising=False)
    _session_id.set(None)
    _replay_context_id.set(None)

    with caplog.at_level("WARNING"):
        _install._bootstrap_replay_context_from_env()

    assert _session_id.get() is None
    assert _replay_context_id.get() is None
    assert any(
        "must be set together" in r.message for r in caplog.records
    ), f"records={[r.message for r in caplog.records]}"
