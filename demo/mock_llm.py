"""
Mock LLM server that simulates an OpenAI-compatible API.

This produces a deterministic multi-step agent scenario:
  Step 1: Agent is asked about Tokyo population → decides to call web_search
  Step 2: (tool result delivered by the agent script)
  Step 3: Agent processes result → decides to search for decade trend
  Step 4: (tool result delivered by the agent script — THIS ONE IS STALE)
  Step 5: Agent synthesizes → HALLUCINATES because of stale data in context

This lets us demo Rewind without needing real API keys.
"""

import json
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

CALL_COUNT = 0

# Pre-scripted responses for each LLM call
RESPONSES = [
    # Call 1: Agent decides to search
    {
        "id": "chatcmpl-mock-001",
        "object": "chat.completion",
        "model": "gpt-4o-2024-08-06",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": None,
                "tool_calls": [{
                    "id": "call_search_1",
                    "type": "function",
                    "function": {
                        "name": "web_search",
                        "arguments": json.dumps({"query": "Tokyo current population 2024"})
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 156, "completion_tokens": 28, "total_tokens": 184}
    },
    # Call 2: Agent decides to search for decade trend
    {
        "id": "chatcmpl-mock-002",
        "object": "chat.completion",
        "model": "gpt-4o-2024-08-06",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": None,
                "tool_calls": [{
                    "id": "call_search_2",
                    "type": "function",
                    "function": {
                        "name": "web_search",
                        "arguments": json.dumps({"query": "Tokyo population change 2014 to 2024 decade trend"})
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 312, "completion_tokens": 35, "total_tokens": 347}
    },
    # Call 3: Agent synthesizes — HALLUCINATION due to stale data
    {
        "id": "chatcmpl-mock-003",
        "object": "chat.completion",
        "model": "gpt-4o-2024-08-06",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": (
                    "# Tokyo Population Analysis\n\n"
                    "## Current Population\n"
                    "Tokyo's 23 special wards currently have a population of approximately "
                    "**14.2 million** (2024), with the Greater Tokyo Area home to 37.4 million people.\n\n"
                    "## Decade Trend (2014-2024)\n"
                    "Over the past decade, Tokyo has experienced **steady, uninterrupted growth**:\n"
                    "- 2014: 13.35 million\n"
                    "- 2019: 13.96 million\n"
                    "- 2024: 14.2 million (projected, confirmed by current data)\n\n"
                    "This represents a **+6.4% increase** over the decade.\n\n"
                    "*Note: Tokyo's growth has been remarkably consistent, "
                    "with no significant disruptions observed during this period.*"
                ),
                "tool_calls": None
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 520, "completion_tokens": 180, "total_tokens": 700}
    },
]


class MockHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        global CALL_COUNT

        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length)

        if CALL_COUNT < len(RESPONSES):
            response = RESPONSES[CALL_COUNT]
            CALL_COUNT += 1
        else:
            # Extra calls get a generic response
            response = {
                "id": f"chatcmpl-mock-extra-{CALL_COUNT}",
                "object": "chat.completion",
                "model": "gpt-4o-2024-08-06",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "I don't have more information."},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}
            }
            CALL_COUNT += 1

        resp_bytes = json.dumps(response).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(resp_bytes)))
        self.end_headers()
        self.wfile.write(resp_bytes)

    def log_message(self, format, *args):
        # Suppress default logging
        pass


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9999
    server = HTTPServer(("127.0.0.1", port), MockHandler)
    print(f"  Mock LLM server on http://127.0.0.1:{port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
