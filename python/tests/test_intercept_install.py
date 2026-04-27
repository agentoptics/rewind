"""Tests for ``rewind_agent.intercept._install``.

The orchestrator's contract: idempotent, missing-library tolerant,
applies custom predicates uniformly to every adapter that gets patched.
"""

from __future__ import annotations

import unittest

from rewind_agent.intercept import (
    DefaultPredicates,
    install,
    is_installed,
    uninstall,
)
from rewind_agent.intercept import (
    aiohttp_middleware,
    httpx_transport,
    requests_adapter,
)


class TestInstallLifecycle(unittest.TestCase):
    def setUp(self) -> None:
        # Defensive: a previous test that crashed mid-install would
        # leave global patches active.
        uninstall()

    def tearDown(self) -> None:
        uninstall()

    def test_install_patches_all_available_adapters(self) -> None:
        self.assertFalse(is_installed())
        install()
        self.assertTrue(is_installed())
        # All three are installed in the test env.
        self.assertTrue(httpx_transport.is_patched())
        self.assertTrue(requests_adapter.is_patched())
        self.assertTrue(aiohttp_middleware.is_patched())

    def test_install_is_idempotent(self) -> None:
        install()
        install()  # no error, no double-patch
        install()
        self.assertTrue(is_installed())
        # All three still patched (not nested / re-wrapped).
        self.assertTrue(httpx_transport.is_patched())
        self.assertTrue(requests_adapter.is_patched())
        self.assertTrue(aiohttp_middleware.is_patched())

    def test_uninstall_clears_all_adapters(self) -> None:
        install()
        uninstall()
        self.assertFalse(is_installed())
        self.assertFalse(httpx_transport.is_patched())
        self.assertFalse(requests_adapter.is_patched())
        self.assertFalse(aiohttp_middleware.is_patched())

    def test_uninstall_without_install_is_safe(self) -> None:
        # No exception; just a no-op.
        uninstall()
        self.assertFalse(is_installed())


class TestCustomPredicates(unittest.TestCase):
    def setUp(self) -> None:
        uninstall()

    def tearDown(self) -> None:
        uninstall()

    def test_install_accepts_custom_predicates(self) -> None:
        # Custom predicates that match nothing should still install
        # cleanly. The patch is at the transport layer; predicates are
        # invoked per-request.
        class NoMatchPredicates(DefaultPredicates):
            def is_llm_call(self, req):  # type: ignore[no-untyped-def]
                return False

        install(predicates=NoMatchPredicates())
        self.assertTrue(is_installed())
        # All adapters report patched.
        self.assertTrue(httpx_transport.is_patched())
        self.assertTrue(requests_adapter.is_patched())
        self.assertTrue(aiohttp_middleware.is_patched())

    def test_install_with_custom_predicates_applies_to_httpx(self) -> None:
        # Custom predicate that matches example.com (which the default
        # never would). Verify the custom predicate is what got bound.
        from unittest.mock import patch as mock_patch

        from rewind_agent.intercept import _flow, _savings

        class ExamplePredicates(DefaultPredicates):
            def is_llm_call(self, req):  # type: ignore[no-untyped-def]
                return "example.com" in req.url_parts.netloc

        install(predicates=ExamplePredicates())

        # Stub ExplicitClient so a record_llm_call attempt is observable.
        _flow.reset_client()
        _savings.reset()
        from rewind_agent.explicit import ExplicitClient

        recorded: list = []
        with mock_patch.object(
            ExplicitClient, "get_replayed_response", return_value=None
        ), mock_patch.object(
            ExplicitClient,
            "record_llm_call",
            side_effect=lambda *a, **kw: recorded.append(kw) or 1,
        ):
            import httpx

            def upstream_handler(request: httpx.Request) -> httpx.Response:
                return httpx.Response(
                    200,
                    json={
                        "choices": [{"message": {"content": "hi"}}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
                        "model": "test",
                    },
                )

            client = httpx.Client(transport=httpx.MockTransport(upstream_handler))
            # api.openai.com — would match default, but our custom
            # predicate explicitly only matches example.com. So this
            # should NOT be recorded.
            client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "test", "messages": []},
            )
            self.assertEqual(len(recorded), 0, "default-host should be skipped")

            # example.com — our custom predicate matches.
            client.post("https://api.example.com/anything", json={"x": 1})
            # Hmm — api.example.com matches the substring "example.com",
            # so it should record.
            self.assertEqual(len(recorded), 1, "example-host should record")

        _flow.reset_client()
        _savings.reset()


if __name__ == "__main__":
    unittest.main()
