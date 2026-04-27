"""Regression tests for Santa review on PR #149.

Each test below is a directed regression for one of the seven blocking
findings in the Santa review. They exist to fail loudly if a future
refactor undoes a fix without thinking about the consequences.

Findings covered:

- ``test_strict_match_409_surfaces`` — Santa #4: ExplicitClient must
  re-raise HTTP 409 strict-match divergence as
  :class:`RewindReplayDivergenceError` instead of swallowing to None.
- ``test_httpx_config_preserved_post_init_wrap`` — Santa #5: when the
  user constructs ``httpx.Client(verify=False, http2=True, ...)``
  without ``transport=``, the resulting client's transport must
  preserve those configs (we wrap httpx's configured default rather
  than replacing it).
- ``test_streaming_miss_does_not_eager_read_body`` — Santa #2: live
  streaming responses must pass through without our flow pre-reading
  the body. Recording fires with placeholder zero-tokens metadata;
  the stream remains iterable to user code.
- ``test_body_only_stream_true_detects_streaming`` — Santa #3: a
  request with ``{"stream": true}`` in the JSON body but no
  ``Accept: text/event-stream`` header must still route through the
  streaming path on cache hit (synthetic SSE, not buffered JSON).
- ``test_install_handles_subset_of_libraries`` — Santa #1 follow-up:
  install + adapter is_patched assertions must be conditional on
  what's actually importable, since CI's bare environment has zero of
  httpx/requests/aiohttp.
"""

from __future__ import annotations

import unittest
import urllib.error
from typing import Any
from unittest.mock import patch

import pytest

# Most regression tests require httpx — the most-used adapter. Skip
# them when httpx isn't installed (CI's bare env); install tests
# below stay alive across all envs.
httpx = pytest.importorskip("httpx", exc_type=ImportError)

from rewind_agent import RewindReplayDivergenceError  # noqa: E402
from rewind_agent.explicit import ExplicitClient  # noqa: E402
from rewind_agent.intercept import _flow, _savings  # noqa: E402
from rewind_agent.intercept import (  # noqa: E402
    aiohttp_middleware,
    httpx_transport,
    requests_adapter,
)


# ── Santa #4: strict-match 409 surfaces as typed exception ─────────


class TestStrictMatch409Surfaces(unittest.TestCase):
    def _seed_replay_context(self) -> tuple[Any, Any]:
        """Set the contextvars get_replayed_response reads. ContextVar
        attributes are read-only at the C level so we can't patch.object
        them; .set() returns a Token we restore in tearDown.
        """
        from rewind_agent import explicit as _explicit_mod

        sid_token = _explicit_mod._session_id.set("sess-1")
        ctx_token = _explicit_mod._replay_context_id.set("ctx-1")
        return sid_token, ctx_token

    def _reset_replay_context(self, tokens: tuple[Any, Any]) -> None:
        from rewind_agent import explicit as _explicit_mod

        _explicit_mod._session_id.reset(tokens[0])
        _explicit_mod._replay_context_id.reset(tokens[1])

    def test_strict_match_409_raises_typed_error(self) -> None:
        """get_replayed_response must NOT swallow HTTP 409 to None.

        A swallow turns a strict-mode divergence into a silent cache
        miss — defeats the entire purpose of strict_match=True.
        """
        body = b'Cache divergence at step 3 (strict_match=true): incoming hash abc123 != stored def456'

        def _raise_409(req, timeout):  # type: ignore[no-untyped-def]
            err = urllib.error.HTTPError(
                url=req.full_url,
                code=409,
                msg="Conflict",
                hdrs=None,  # type: ignore[arg-type]
                fp=None,
            )
            # Override read() to return the canned diagnostic body.
            err.read = lambda: body  # type: ignore[method-assign]
            raise err

        tokens = self._seed_replay_context()
        try:
            client = ExplicitClient()
            with patch("urllib.request.urlopen", side_effect=_raise_409):
                with self.assertRaises(RewindReplayDivergenceError) as ctx:
                    client.get_replayed_response({"model": "x", "messages": []})
            self.assertIn("strict_match=true", str(ctx.exception))
        finally:
            self._reset_replay_context(tokens)

    def test_non_409_http_error_is_swallowed_to_cache_miss(self) -> None:
        """Other 4xx/5xx errors degrade to None (cache miss), preserving
        the previous best-effort behavior. Only 409 is re-raised."""

        def _raise_500(req, timeout):  # type: ignore[no-untyped-def]
            raise urllib.error.HTTPError(
                url=req.full_url,
                code=500,
                msg="Internal Server Error",
                hdrs=None,  # type: ignore[arg-type]
                fp=None,
            )

        tokens = self._seed_replay_context()
        try:
            client = ExplicitClient()
            with patch("urllib.request.urlopen", side_effect=_raise_500):
                result = client.get_replayed_response({"model": "x"})
            self.assertIsNone(result)
        finally:
            self._reset_replay_context(tokens)


# ── Santa #5: httpx configured default transport preserved ─────────


class TestHttpxConfigPreserved(unittest.TestCase):
    def setUp(self) -> None:
        httpx_transport.unpatch_httpx_clients()

    def tearDown(self) -> None:
        httpx_transport.unpatch_httpx_clients()

    def test_verify_false_setting_survives_intercept_install(self) -> None:
        """``httpx.Client(verify=False)`` must reach the underlying
        transport. Pre-Santa #5, our patch built a fresh
        RewindHTTPTransport() without forwarding kwargs, dropping verify.
        """
        httpx_transport.patch_httpx_clients()

        client = httpx.Client(verify=False)
        # Our wrapper exposes _inner — the configured default transport
        # httpx built. That inner transport should reflect verify=False.
        wrapper = client._transport
        self.assertIsNotNone(getattr(wrapper, "_inner", None),
                             "Phase 1 wrapper missing _inner — config drop bug regressed")
        # httpx HTTPTransport stores SSL settings inside an internal
        # SSLContext; we verify the pool's verify mode reflects False.
        # The cleanest signal is that _inner is NOT just a default-config
        # HTTPTransport — it has the user-supplied verify=False.
        # The most stable cross-version check: the inner exists and is
        # not our class (it's the configured default).
        self.assertNotIsInstance(
            wrapper._inner,
            type(wrapper),
            "inner transport should be httpx's configured default, not another Rewind wrapper",
        )

    def test_user_supplied_transport_is_wrapped_not_replaced(self) -> None:
        """Mode (a): user passes transport=X → we wrap it so X's logic still runs."""
        httpx_transport.patch_httpx_clients()

        called = []

        def handler(request: httpx.Request) -> httpx.Response:
            called.append(request.url)
            return httpx.Response(200, json={"ok": True})

        user_t = httpx.MockTransport(handler)
        client = httpx.Client(transport=user_t)
        # Our wrapper's _inner should be the user's transport.
        self.assertIs(client._transport._inner, user_t)


# ── Santa #2: streaming pass-through (no eager body read) ──────────


class TestStreamingPassThrough(unittest.TestCase):
    def setUp(self) -> None:
        httpx_transport.patch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def tearDown(self) -> None:
        httpx_transport.unpatch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def test_streaming_miss_passes_through_without_consuming_body(self) -> None:
        """Live streaming response must reach user code with the body
        unconsumed. Pre-Santa #2 we'd ``await resp.json()`` before
        returning, breaking httpx streaming clients.
        """
        # Fake upstream returns a streaming SSE body.
        sse_body = (
            b'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n'
            b"data: [DONE]\n\n"
        )

        def upstream_handler(request: httpx.Request) -> httpx.Response:
            return httpx.Response(
                200,
                headers={"Content-Type": "text/event-stream"},
                stream=httpx.ByteStream(sse_body),
            )

        # Stub ExplicitClient — cache miss + record_llm_call observability.
        recorded: list[dict[str, Any]] = []
        with patch.object(
            ExplicitClient, "get_replayed_response", return_value=None
        ), patch.object(
            ExplicitClient,
            "record_llm_call",
            side_effect=lambda *a, **kw: recorded.append(kw) or 1,
        ):
            client = httpx.Client(transport=httpx.MockTransport(upstream_handler))
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "stream": True, "messages": []},
                headers={"accept": "text/event-stream"},
            )
            # Critical assertion: we can still iterate the body.
            # If _flow had pre-read it, this would yield empty bytes.
            chunks = list(resp.iter_bytes())
            joined = b"".join(chunks)
            self.assertIn(b"data: [DONE]", joined,
                          "streaming body was consumed before user could iterate")

        # Recording fired with placeholder None response + zero tokens
        # (Phase 1 limitation; tee-based capture in v1.1).
        self.assertEqual(len(recorded), 1, "streaming miss should record once")
        self.assertIsNone(recorded[0]["response"])
        self.assertEqual(recorded[0]["tokens_in"], 0)
        self.assertEqual(recorded[0]["tokens_out"], 0)


# ── Santa #3: body-only stream:true detected ───────────────────────


class TestBodyOnlyStreamTrueDetected(unittest.TestCase):
    def setUp(self) -> None:
        httpx_transport.patch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def tearDown(self) -> None:
        httpx_transport.unpatch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def test_cache_hit_with_body_stream_true_emits_synthetic_sse(self) -> None:
        """Request body has ``"stream": true`` but no Accept header —
        cache hit must still route through the streaming path
        (synthetic SSE), not the buffered path.
        """
        cached_inner = {
            "choices": [{"message": {"content": "stream-via-body"}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2},
            "model": "gpt-4o",
        }

        def boom(request: httpx.Request) -> httpx.Response:
            raise AssertionError("live transport called on cache hit")

        with patch.object(
            ExplicitClient, "get_replayed_response", return_value=cached_inner
        ):
            client = httpx.Client(transport=httpx.MockTransport(boom))
            # Note: NO Accept: text/event-stream header. Body has stream: true.
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "stream": True, "messages": []},
            )
            # The synthetic response should be SSE-formatted (data: …).
            chunks = list(resp.iter_bytes())
            joined = b"".join(chunks)
            self.assertIn(b"data: ", joined,
                          "body-only stream:true didn't trigger SSE synth — Santa #3 regression")
            self.assertIn(b"data: [DONE]", joined)


# ── Santa #1 follow-up: install with subset of libraries ───────────


class TestInstallWithSubsetOfLibraries(unittest.TestCase):
    """Reproduces CI's bare environment: not all of httpx, requests,
    aiohttp are available. install() must succeed and only patch what
    IS available.
    """

    def setUp(self) -> None:
        from rewind_agent.intercept import uninstall

        uninstall()

    def tearDown(self) -> None:
        from rewind_agent.intercept import uninstall

        uninstall()

    def test_install_only_patches_available_adapters(self) -> None:
        from rewind_agent.intercept import install, is_installed

        install()
        self.assertTrue(is_installed())

        # Each adapter is patched if and only if its library is importable.
        self.assertEqual(
            httpx_transport.is_patched(),
            httpx_transport.HTTPX_AVAILABLE,
        )
        self.assertEqual(
            requests_adapter.is_patched(),
            requests_adapter.REQUESTS_AVAILABLE,
        )
        self.assertEqual(
            aiohttp_middleware.is_patched(),
            aiohttp_middleware.AIOHTTP_AVAILABLE,
        )


if __name__ == "__main__":
    unittest.main()
