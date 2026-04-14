"""Tests for the rewind fix diagnosis module."""

import json
import os
import subprocess
import sys
import unittest
from unittest.mock import MagicMock, patch

from rewind_agent.fix import (
    VALID_FIX_TYPES,
    _build_step_summary,
    _build_preceding_context,
    _to_str,
    run_diagnosis,
)


class TestHelpers(unittest.TestCase):
    def test_to_str_none(self):
        self.assertEqual(_to_str(None), "(empty)")

    def test_to_str_string(self):
        self.assertEqual(_to_str("hello"), "hello")

    def test_to_str_dict(self):
        result = _to_str({"key": "val"})
        self.assertIn("key", result)
        self.assertIn("val", result)

    def test_to_str_truncation(self):
        long = "x" * 1000
        result = _to_str(long, max_len=100)
        self.assertIn("truncated", result)
        self.assertTrue(len(result) < 1000)

    def test_to_str_no_truncation_when_short(self):
        short = "hello"
        result = _to_str(short, max_len=100)
        self.assertEqual(result, "hello")
        self.assertNotIn("truncated", result)


class TestBuildStepSummary(unittest.TestCase):
    def test_empty_steps(self):
        self.assertEqual(_build_step_summary([]), "")

    def test_success_step(self):
        steps = [{"step_number": 1, "step_type": "llm_call", "status": "success",
                  "model": "gpt-4o", "tokens_in": 100, "tokens_out": 50,
                  "duration_ms": 1200, "tool_name": None, "error": None}]
        result = _build_step_summary(steps)
        self.assertIn("✓", result)
        self.assertIn("Step 1", result)
        self.assertIn("gpt-4o", result)

    def test_error_step(self):
        steps = [{"step_number": 3, "step_type": "llm_call", "status": "error",
                  "model": "gpt-4o-mini", "tokens_in": 4200, "tokens_out": 0,
                  "duration_ms": 800, "tool_name": None, "error": "Context length exceeded"}]
        result = _build_step_summary(steps)
        self.assertIn("✗", result)
        self.assertIn("Context length exceeded", result)

    def test_tool_name_shown(self):
        steps = [{"step_number": 2, "step_type": "tool_call", "status": "success",
                  "model": "gpt-4o", "tokens_in": 0, "tokens_out": 0,
                  "duration_ms": 50, "tool_name": "Read", "error": None}]
        result = _build_step_summary(steps)
        self.assertIn("[Read]", result)


class TestBuildPrecedingContext(unittest.TestCase):
    def test_empty(self):
        result = _build_preceding_context([])
        self.assertIn("no preceding", result)

    def test_with_steps(self):
        preceding = [
            {"step_number": 5, "request": {"model": "gpt-4o"}, "response": {"choices": []}},
        ]
        result = _build_preceding_context(preceding)
        self.assertIn("Step 5", result)
        self.assertIn("gpt-4o", result)


class TestRunDiagnosis(unittest.TestCase):
    """Test run_diagnosis with mocked OpenAI client."""

    def _make_payload(self, error="Context length exceeded", expected=None):
        return {
            "session": {"id": "s1", "name": "test-agent", "status": "failed", "total_steps": 5},
            "steps": [
                {"step_number": 1, "step_type": "llm_call", "status": "success",
                 "model": "gpt-4o-mini", "tokens_in": 100, "tokens_out": 50,
                 "duration_ms": 1000, "tool_name": None, "error": None},
                {"step_number": 2, "step_type": "tool_call", "status": "success",
                 "model": "gpt-4o-mini", "tokens_in": 0, "tokens_out": 0,
                 "duration_ms": 50, "tool_name": "Read", "error": None},
                {"step_number": 3, "step_type": "llm_call", "status": "error",
                 "model": "gpt-4o-mini", "tokens_in": 4200, "tokens_out": 0,
                 "duration_ms": 800, "tool_name": None, "error": error},
            ],
            "failure_step": 3,
            "failure_context": {
                "request": {"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "test"}]},
                "response": None,
                "preceding_steps": [
                    {"step_number": 2, "request": {"tool": "Read"}, "response": {"content": "file data"}},
                ],
            },
            "expected": expected,
            "config": {"model": "gpt-4o-mini"},
        }

    def _mock_openai_response(self, fix_type="swap_model", model="gpt-4o"):
        """Build a mock OpenAI response with function calling."""
        tool_call = MagicMock()
        tool_call.function.arguments = json.dumps({
            "root_cause": "Context window exceeded due to large tool response",
            "failed_step": 3,
            "fork_from": 2,
            "fix_type": fix_type,
            "fix_params": {"model": model} if fix_type == "swap_model" else {},
            "explanation": "Switching to a model with better long-context handling",
            "confidence": "high",
        })

        message = MagicMock()
        message.tool_calls = [tool_call]
        message.content = None

        choice = MagicMock()
        choice.message = message

        response = MagicMock()
        response.choices = [choice]
        return response

    @patch("rewind_agent.fix._get_openai_client")
    def test_successful_diagnosis(self, mock_get_client):
        client = MagicMock()
        client.chat.completions.create.return_value = self._mock_openai_response()
        mock_get_client.return_value = client

        result = run_diagnosis(self._make_payload())

        self.assertEqual(result["fix_type"], "swap_model")
        self.assertEqual(result["failed_step"], 3)
        self.assertEqual(result["fork_from"], 2)
        self.assertEqual(result["confidence"], "high")
        self.assertIn("Context window", result["root_cause"])

    @patch("rewind_agent.fix._get_openai_client")
    def test_retry_step_diagnosis(self, mock_get_client):
        client = MagicMock()
        client.chat.completions.create.return_value = self._mock_openai_response(
            fix_type="retry_step"
        )
        mock_get_client.return_value = client

        result = run_diagnosis(self._make_payload())
        self.assertEqual(result["fix_type"], "retry_step")

    @patch("rewind_agent.fix._get_openai_client")
    def test_no_fix_diagnosis(self, mock_get_client):
        client = MagicMock()
        client.chat.completions.create.return_value = self._mock_openai_response(
            fix_type="no_fix"
        )
        mock_get_client.return_value = client

        result = run_diagnosis(self._make_payload())
        self.assertEqual(result["fix_type"], "no_fix")

    @patch("rewind_agent.fix._get_openai_client")
    def test_invalid_fix_type_falls_back_to_no_fix(self, mock_get_client):
        client = MagicMock()
        client.chat.completions.create.return_value = self._mock_openai_response(
            fix_type="invented_type"
        )
        mock_get_client.return_value = client

        result = run_diagnosis(self._make_payload())
        self.assertEqual(result["fix_type"], "no_fix")

    @patch("rewind_agent.fix._get_openai_client")
    def test_no_tool_call_returns_no_fix(self, mock_get_client):
        client = MagicMock()
        message = MagicMock()
        message.tool_calls = []
        message.content = "I think the problem is..."
        choice = MagicMock()
        choice.message = message
        response = MagicMock()
        response.choices = [choice]
        client.chat.completions.create.return_value = response
        mock_get_client.return_value = client

        result = run_diagnosis(self._make_payload())
        self.assertEqual(result["fix_type"], "no_fix")
        self.assertIn("did not use function calling", result["root_cause"])

    @patch("rewind_agent.fix._get_openai_client")
    def test_expected_field_included_in_prompt(self, mock_get_client):
        client = MagicMock()
        client.chat.completions.create.return_value = self._mock_openai_response()
        mock_get_client.return_value = client

        run_diagnosis(self._make_payload(expected="Should book a restaurant"))

        call_args = client.chat.completions.create.call_args
        prompt_content = call_args.kwargs["messages"][0]["content"]
        self.assertIn("Should book a restaurant", prompt_content)
        self.assertIn("Expected Behavior", prompt_content)

    @patch("rewind_agent.fix._get_openai_client")
    def test_api_failure_returns_graceful_result(self, mock_get_client):
        client = MagicMock()
        client.chat.completions.create.side_effect = ValueError("API error")
        mock_get_client.return_value = client

        result = run_diagnosis(self._make_payload())
        self.assertEqual(result["fix_type"], "no_fix")
        self.assertIn("API error", result["root_cause"])
        self.assertEqual(result["confidence"], "low")

    def test_valid_fix_types(self):
        self.assertIn("swap_model", VALID_FIX_TYPES)
        self.assertIn("inject_system", VALID_FIX_TYPES)
        self.assertIn("adjust_temperature", VALID_FIX_TYPES)
        self.assertIn("retry_step", VALID_FIX_TYPES)
        self.assertIn("no_fix", VALID_FIX_TYPES)
        self.assertEqual(len(VALID_FIX_TYPES), 5)


class TestSubprocessEntryPoint(unittest.TestCase):
    """Test the subprocess protocol by invoking fix.py as a module."""

    def test_invalid_json_returns_error(self):
        result = subprocess.run(
            [sys.executable, "-m", "rewind_agent.fix"],
            input="not json",
            capture_output=True, text=True,
            cwd=str(__import__("pathlib").Path(__file__).parent.parent),
        )
        self.assertEqual(result.returncode, 1)
        output = json.loads(result.stdout)
        self.assertIn("Invalid stdin JSON", output["root_cause"])
        self.assertEqual(output["fix_type"], "no_fix")

    def test_missing_api_key_returns_error(self):
        payload = {
            "session": {"id": "s1", "name": "test", "status": "failed", "total_steps": 1},
            "steps": [{"step_number": 1, "step_type": "llm_call", "status": "error",
                       "model": "gpt-4o", "tokens_in": 0, "tokens_out": 0,
                       "duration_ms": 0, "tool_name": None, "error": "test"}],
            "failure_step": 1,
            "failure_context": {"request": None, "response": None, "preceding_steps": []},
            "config": {"api_key_env": "REWIND_TEST_NONEXISTENT_KEY"},
        }
        env = {k: v for k, v in os.environ.items() if k != "OPENAI_API_KEY"}
        result = subprocess.run(
            [sys.executable, "-m", "rewind_agent.fix"],
            input=json.dumps(payload),
            capture_output=True, text=True, env=env,
            cwd=str(__import__("pathlib").Path(__file__).parent.parent),
        )
        self.assertEqual(result.returncode, 1)
        output = json.loads(result.stdout)
        self.assertIn("config error", output["root_cause"])


if __name__ == "__main__":
    unittest.main()
