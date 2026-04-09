"""
Basic Recording — Record 3 LLM calls through the Rewind proxy.

Setup:
    # Terminal 1: Start the proxy
    rewind record --name "basic-demo" --upstream https://api.openai.com

    # Terminal 2: Run this script
    export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
    python examples/01_basic_recording.py

    # After it finishes:
    rewind show latest
    rewind inspect latest
"""

import openai

client = openai.OpenAI()  # Picks up OPENAI_BASE_URL from env

# Step 1: Ask a question
resp1 = client.chat.completions.create(
    model="gpt-4o",
    messages=[{"role": "user", "content": "What is the capital of France?"}],
    max_tokens=50,
)
print(f"Step 1: {resp1.choices[0].message.content}")

# Step 2: Follow up
resp2 = client.chat.completions.create(
    model="gpt-4o",
    messages=[
        {"role": "user", "content": "What is the capital of France?"},
        {"role": "assistant", "content": resp1.choices[0].message.content},
        {"role": "user", "content": "What is its population?"},
    ],
    max_tokens=100,
)
print(f"Step 2: {resp2.choices[0].message.content}")

# Step 3: Summarize
resp3 = client.chat.completions.create(
    model="gpt-4o",
    messages=[
        {"role": "user", "content": "Summarize: Paris is the capital of France with a population of about 2.1 million."},
    ],
    max_tokens=50,
)
print(f"Step 3: {resp3.choices[0].message.content}")

print("\nDone! Run 'rewind show latest' to see the trace.")
