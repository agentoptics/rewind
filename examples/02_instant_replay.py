"""
Instant Replay — Run the same request twice, second time is free.

Setup:
    # Terminal 1: Start the proxy WITH --replay
    rewind record --name "replay-demo" --upstream https://api.openai.com --replay

    # Terminal 2: Run this script
    export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
    python examples/02_instant_replay.py

    # After it finishes:
    rewind cache    # see savings
"""

import openai
import time

client = openai.OpenAI()

messages = [{"role": "user", "content": "Explain quantum computing in one sentence."}]

# First call — cache miss, hits the real API
print("Call 1 (cache miss)...", end=" ", flush=True)
start = time.time()
resp1 = client.chat.completions.create(model="gpt-4o", messages=messages, max_tokens=50)
t1 = time.time() - start
print(f"{t1:.2f}s — {resp1.choices[0].message.content}")

# Second call — IDENTICAL request, cache hit, instant and free
print("Call 2 (cache hit)... ", end=" ", flush=True)
start = time.time()
resp2 = client.chat.completions.create(model="gpt-4o", messages=messages, max_tokens=50)
t2 = time.time() - start
print(f"{t2:.2f}s — {resp2.choices[0].message.content}")

print(f"\nSpeedup: {t1/t2:.0f}x faster on cache hit")
print("Run 'rewind cache' to see total savings.")
