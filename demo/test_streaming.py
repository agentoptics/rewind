#!/usr/bin/env python3
"""
Test streaming through Rewind proxy using OpenAI-compatible SSE format.

Uses a mock SSE server to verify:
  1. Chunks arrive at the client in real-time (not buffered)
  2. Rewind records the full assembled response after stream ends
"""

import json
import sys
import time
import threading
import urllib.request
from http.server import HTTPServer, BaseHTTPRequestHandler

# ── Mock SSE LLM Server ────────────────────────────────────────

SSE_CHUNKS = [
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":" there"},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"! How"},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":" can"},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":" I help"},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":" you"},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":" today?"},"finish_reason":null}]}\n\n',
    'data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":9,"total_tokens":21}}\n\n',
    'data: [DONE]\n\n',
]


class SSEHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(content_length)

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()

        for chunk in SSE_CHUNKS:
            self.wfile.write(chunk.encode())
            self.wfile.flush()
            time.sleep(0.1)  # simulate token-by-token latency

    def log_message(self, format, *args):
        pass


# ── Colors ──────────────────────────────────────────────────────
C = "\033[36m"; G = "\033[32m"; Y = "\033[33m"; R = "\033[31m"
D = "\033[2m"; B = "\033[1m"; X = "\033[0m"


def main():
    # Start mock SSE server
    mock_server = HTTPServer(("127.0.0.1", 9998), SSEHandler)
    t = threading.Thread(target=mock_server.serve_forever, daemon=True)
    t.start()
    print(f"  {D}Mock SSE server on :9998{X}")

    proxy_url = "http://127.0.0.1:8443"

    print()
    print(f"  {C}{B}⏪ Streaming Test{X}")
    print(f"  {D}Sending stream:true request through Rewind proxy...{X}")
    print()

    payload = json.dumps({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "Say hello"}],
        "stream": True,
    }).encode()

    req = urllib.request.Request(
        f"{proxy_url}/v1/chat/completions",
        data=payload,
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer sk-mock",
        },
    )

    try:
        start = time.time()
        with urllib.request.urlopen(req) as resp:
            print(f"  {Y}▶ Receiving stream:{X} ", end="", flush=True)
            full_response = b""
            chunk_count = 0
            while True:
                chunk = resp.read(1024)
                if not chunk:
                    break
                full_response += chunk
                chunk_count += 1
                # Show dots as chunks arrive
                print(f"{G}·{X}", end="", flush=True)

            elapsed = time.time() - start
            print()
            print()

        # Parse the SSE events from the response
        assembled_text = ""
        for line in full_response.decode().split("\n"):
            if line.startswith("data: ") and line != "data: [DONE]":
                try:
                    event = json.loads(line[6:])
                    content = event.get("choices", [{}])[0].get("delta", {}).get("content", "")
                    assembled_text += content
                except:
                    pass

        print(f"  {G}{B}✓ Stream received!{X}")
        print(f"  {D}  Chunks: {chunk_count}{X}")
        print(f"  {D}  Time: {elapsed:.2f}s{X}")
        print(f"  {D}  Assembled text: \"{assembled_text}\"{X}")
        print()
        print(f"  {C}Rewind should have recorded the full assembled response.{X}")
        print(f"  {C}Check: {G}rewind show latest{X}")
        print()

    except Exception as e:
        print(f"\n  {R}Error: {e}{X}")
        print(f"  {D}Make sure the Rewind proxy is running:{X}")
        print(f"  {G}  rewind record --name stream-test --upstream http://127.0.0.1:9998{X}")

    mock_server.shutdown()


if __name__ == "__main__":
    main()
