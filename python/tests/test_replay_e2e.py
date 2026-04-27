"""
End-to-end replay test — records a multi-step agent session against a mock LLM,
then replays from a fork point and verifies:
  1. Cached steps are served without hitting the LLM
  2. Live steps after the fork point go to the real LLM
  3. The forked timeline has correct structure in the store
  4. Savings counters (tokens, cost, time) are accurate

Uses a real OpenAI client pointed at an in-process mock server, so no API
keys or external dependencies are needed.
"""

import json
import os
import sqlite3
import tempfile
import threading
import unittest
from http.server import HTTPServer, BaseHTTPRequestHandler

import openai

from rewind_agent.store import Store
from rewind_agent.recorder import Recorder, estimate_cost

MOCK_PORT = 19876


RESPONSES = [
    {
        "id": "chatcmpl-step1",
        "object": "chat.completion",
        "created": 1700000001,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Step 1 answer: Tokyo has 14M people."},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 100, "completion_tokens": 30, "total_tokens": 130},
    },
    {
        "id": "chatcmpl-step2",
        "object": "chat.completion",
        "created": 1700000002,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Step 2 answer: Population grew 6% over the decade."},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 200, "completion_tokens": 40, "total_tokens": 240},
    },
    {
        "id": "chatcmpl-step3",
        "object": "chat.completion",
        "created": 1700000003,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Step 3 answer: Summary of Tokyo population."},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 300, "completion_tokens": 50, "total_tokens": 350},
    },
    {
        "id": "chatcmpl-step3-replay",
        "object": "chat.completion",
        "created": 1700000004,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Step 3 REPLAYED: Better summary with corrected data."},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 300, "completion_tokens": 55, "total_tokens": 355},
    },
]


class _MockLLMHandler(BaseHTTPRequestHandler):
    call_log = []

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length)
        request = json.loads(body) if body else {}

        idx = len(_MockLLMHandler.call_log)
        _MockLLMHandler.call_log.append(request)

        response = RESPONSES[min(idx, len(RESPONSES) - 1)]
        resp_bytes = json.dumps(response).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(resp_bytes)))
        self.end_headers()
        self.wfile.write(resp_bytes)

    def log_message(self, format, *args):
        pass


def _start_mock_server():
    server = HTTPServer(("127.0.0.1", MOCK_PORT), _MockLLMHandler)
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    return server


def _make_client():
    return openai.OpenAI(
        api_key="mock-key",
        base_url=f"http://127.0.0.1:{MOCK_PORT}/v1",
    )


def _get_content(response):
    """Extract content from a response (works with ChatCompletion, SimpleNamespace, or dict)."""
    choice = response.choices[0]
    if hasattr(choice, "message"):
        msg = choice.message
        return msg.content if hasattr(msg, "content") else msg.get("content")
    return choice["message"]["content"]


def _run_3step_agent(client):
    """Simulate a 3-step agent: ask, follow-up, summarize."""
    r1 = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "What is Tokyo's population?"}],
    )
    r2 = client.chat.completions.create(
        model="gpt-4o",
        messages=[
            {"role": "user", "content": "What is Tokyo's population?"},
            {"role": "assistant", "content": _get_content(r1)},
            {"role": "user", "content": "What about the decade trend?"},
        ],
    )
    r3 = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "Summarize Tokyo population data."}],
    )
    return [r1, r2, r3]


class TestReplayE2E(unittest.TestCase):
    """Full record → replay → verify cycle using a mock LLM server."""

    @classmethod
    def setUpClass(cls):
        cls._server = _start_mock_server()

    @classmethod
    def tearDownClass(cls):
        cls._server.shutdown()

    def setUp(self):
        _MockLLMHandler.call_log = []
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)

    def tearDown(self):
        self.store.close()

    def _record_original_session(self):
        """Phase 1: record 3 steps on the main timeline."""
        sid, tid = self.store.create_session("replay-e2e-test")
        recorder = Recorder(self.store, sid, tid)
        recorder.patch_all()
        try:
            client = _make_client()
            results = _run_3step_agent(client)
        finally:
            recorder.unpatch_all()
        return sid, tid, results

    def _replay_from(self, session_id, fork_at_step):
        """Phase 2: fork and replay from a given step."""
        root_tl = self.store.get_root_timeline(session_id)
        parent_steps = self.store.get_full_timeline_steps(root_tl["id"], session_id)
        fork_tid = self.store.create_fork_timeline(
            session_id, root_tl["id"], fork_at_step, "replayed"
        )
        recorder = Recorder(
            self.store, session_id, fork_tid,
            replay_steps=parent_steps, fork_at_step=fork_at_step,
        )
        recorder.patch_all()
        try:
            client = _make_client()
            results = _run_3step_agent(client)
        finally:
            recorder.unpatch_all()
        return fork_tid, results, recorder

    # ── Tests ──────────────────────────────────────────────────

    def test_record_then_replay_from_step2(self):
        """Record 3 steps, replay from step 2. Steps 1-2 from cache, step 3 live."""
        sid, tid, original = self._record_original_session()

        # 3 LLM calls for the original recording
        self.assertEqual(len(_MockLLMHandler.call_log), 3)

        # Verify original steps stored
        steps = self.store.get_steps(tid)
        self.assertEqual(len(steps), 3)
        for i, step in enumerate(steps, start=1):
            self.assertEqual(step["step_number"], i)
            self.assertEqual(step["status"], "success")

        # Replay from step 2 — track additional LLM calls
        calls_before = len(_MockLLMHandler.call_log)
        fork_tid, replayed, recorder = self._replay_from(sid, fork_at_step=2)
        live_calls = len(_MockLLMHandler.call_log) - calls_before

        # Only step 3 should have hit the LLM (steps 1-2 served from cache)
        self.assertEqual(
            live_calls, 1,
            f"Expected 1 live LLM call (step 3), got {live_calls}",
        )

        # Steps 1-2 should have been returned from cache
        self.assertEqual(
            _get_content(replayed[0]),
            "Step 1 answer: Tokyo has 14M people.",
        )
        self.assertEqual(
            _get_content(replayed[1]),
            "Step 2 answer: Population grew 6% over the decade.",
        )

        # Step 3 is live — verify it got a different response from the LLM
        step3_content = _get_content(replayed[2])
        self.assertNotEqual(
            step3_content,
            "Step 3 answer: Summary of Tokyo population.",
            "Step 3 should NOT be the original cached response",
        )

        # Fork timeline should only have 1 own step (the live one)
        fork_own_steps = self.store.get_steps(fork_tid)
        self.assertEqual(len(fork_own_steps), 1)
        self.assertEqual(fork_own_steps[0]["step_number"], 3)

        # Full timeline (inherited + own) should show 3 steps
        full_steps = self.store.get_full_timeline_steps(fork_tid, sid)
        self.assertEqual(len(full_steps), 3)

        # Savings: 2 cached steps
        self.assertEqual(recorder._cached_steps_count, 2)
        expected_tokens = (100 + 30) + (200 + 40)  # steps 1 + 2
        self.assertEqual(recorder._cached_tokens, expected_tokens)

    def test_replay_from_step1_caches_only_step1(self):
        """Replay from step 1: only step 1 cached, steps 2-3 live."""
        sid, _, _ = self._record_original_session()
        calls_before = len(_MockLLMHandler.call_log)

        fork_tid, _, recorder = self._replay_from(sid, fork_at_step=1)
        live_calls = len(_MockLLMHandler.call_log) - calls_before

        # Steps 2 and 3 should have hit the LLM
        self.assertEqual(live_calls, 2)
        self.assertEqual(recorder._cached_steps_count, 1)

        # Fork timeline should have 2 own steps
        fork_own_steps = self.store.get_steps(fork_tid)
        self.assertEqual(len(fork_own_steps), 2)
        step_numbers = [s["step_number"] for s in fork_own_steps]
        self.assertEqual(step_numbers, [2, 3])

    def test_replay_from_all_steps_no_live_calls(self):
        """Replay from step 3 (all steps): everything from cache, no LLM calls."""
        sid, _, _ = self._record_original_session()
        calls_before = len(_MockLLMHandler.call_log)

        fork_tid, replayed, recorder = self._replay_from(sid, fork_at_step=3)
        live_calls = len(_MockLLMHandler.call_log) - calls_before

        self.assertEqual(live_calls, 0)
        self.assertEqual(recorder._cached_steps_count, 3)

        # Fork timeline has no own steps
        fork_own_steps = self.store.get_steps(fork_tid)
        self.assertEqual(len(fork_own_steps), 0)

        # All responses should match the original
        self.assertIn("Tokyo has 14M", _get_content(replayed[0]))
        self.assertIn("6% over the decade", _get_content(replayed[1]))
        self.assertIn("Summary of Tokyo", _get_content(replayed[2]))

    def test_replay_savings_cost_tracking(self):
        """Cached steps should track token savings and estimated cost."""
        sid, _, _ = self._record_original_session()

        _, _, recorder = self._replay_from(sid, fork_at_step=2)

        # Tokens saved = step1 (100+30) + step2 (200+40) = 370
        self.assertEqual(recorder._cached_tokens, 370)

        # Cost should be positive
        expected_cost = (
            estimate_cost("gpt-4o", 100, 30) +
            estimate_cost("gpt-4o", 200, 40)
        )
        self.assertAlmostEqual(recorder._cached_cost, expected_cost, places=6)
        self.assertGreater(recorder._cached_cost, 0.0)

        # Duration saved should be non-negative (may be 0 with fast mock server)
        self.assertGreaterEqual(recorder._cached_duration_ms, 0)

    def test_replay_timeline_metadata(self):
        """Forked timeline should reference the parent correctly."""
        sid, tid, _ = self._record_original_session()
        fork_tid, _, _ = self._replay_from(sid, fork_at_step=2)

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT parent_timeline_id, fork_at_step, label FROM timelines WHERE id = ?",
            (fork_tid,),
        ).fetchone()
        conn.close()

        self.assertEqual(row[0], tid, "Fork should reference parent timeline")
        self.assertEqual(row[1], 2, "Fork-at-step should be 2")
        self.assertEqual(row[2], "replayed", "Label should be 'replayed'")

    def test_replay_preserves_response_blobs(self):
        """Cached replay responses should be byte-identical to the originals."""
        sid, tid, _ = self._record_original_session()

        # Read original blobs
        original_steps = self.store.get_steps(tid)
        original_blobs = {}
        for s in original_steps:
            if s["response_blob"]:
                data = self.store.blobs.get(s["response_blob"])
                original_blobs[s["step_number"]] = json.loads(data)

        # Replay from step 2 — steps 1-2 should return identical data
        _, replayed, _ = self._replay_from(sid, fork_at_step=2)

        # The cached response for step 1 should match original blob content
        r1_content = _get_content(replayed[0])
        self.assertEqual(
            r1_content,
            original_blobs[1]["choices"][0]["message"]["content"],
        )

    def test_multiple_sequential_replays(self):
        """Multiple replay forks from the same session should each work independently."""
        sid, _, _ = self._record_original_session()

        # First replay: fork at step 2
        calls_before1 = len(_MockLLMHandler.call_log)
        fork1_tid, _, rec1 = self._replay_from(sid, fork_at_step=2)
        live_calls_1 = len(_MockLLMHandler.call_log) - calls_before1

        # Second replay: fork at step 1
        calls_before2 = len(_MockLLMHandler.call_log)
        fork2_tid, _, rec2 = self._replay_from(sid, fork_at_step=1)
        live_calls_2 = len(_MockLLMHandler.call_log) - calls_before2

        self.assertEqual(live_calls_1, 1)
        self.assertEqual(live_calls_2, 2)
        self.assertNotEqual(fork1_tid, fork2_tid)

        self.assertEqual(rec1._cached_steps_count, 2)
        self.assertEqual(rec2._cached_steps_count, 1)


if __name__ == "__main__":
    unittest.main()
