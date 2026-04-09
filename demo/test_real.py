#!/usr/bin/env python3
"""
Test Rewind with REAL LLM calls through a Bedrock-compatible gateway.

This script:
  1. Calls Claude Sonnet via a Bedrock gateway through the Rewind proxy
  2. Agent does a multi-step reasoning task
  3. All calls are recorded by Rewind

Usage:
  # Terminal 1: Start proxy (replace URL with your Bedrock gateway)
  rewind record --name "real-test" --upstream https://your-bedrock-gateway.example.com/bedrock

  # Terminal 2: Run this script
  BEDROCK_GATEWAY_URL=https://your-bedrock-gateway.example.com/bedrock python3 demo/test_real.py
"""

import json
import os
import ssl
import urllib.request

PROXY_URL = os.environ.get("REWIND_PROXY", "http://127.0.0.1:8443")
API_KEY = os.environ.get("BEDROCK_API_KEY", os.environ.get("ANTHROPIC_API_KEY", ""))
MODEL = "us.anthropic.claude-sonnet-4-20250514-v1:0"

# Colors
C = "\033[36m"; G = "\033[32m"; Y = "\033[33m"; R = "\033[31m"
D = "\033[2m"; B = "\033[1m"; X = "\033[0m"


def call_bedrock(messages, system=None, max_tokens=300):
    """Call Claude via Bedrock gateway through Rewind proxy."""
    payload = {
        "anthropic_version": "bedrock-2023-05-31",
        "max_tokens": max_tokens,
        "messages": messages,
    }
    if system:
        payload["system"] = system

    data = json.dumps(payload).encode()

    # The proxy forwards to upstream + path
    # Bedrock path: /model/{model}/invoke
    url = f"{PROXY_URL}/model/{MODEL}/invoke"

    req = urllib.request.Request(
        url,
        data=data,
        headers={
            "Content-Type": "application/json",
            "x-api-key": API_KEY,
        },
    )

    # Allow self-signed certs for local proxy
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE

    with urllib.request.urlopen(req, context=ctx) as resp:
        body = json.loads(resp.read())
        return body


def extract_text(response):
    """Extract text from Anthropic/Bedrock response."""
    content = response.get("content", [])
    texts = []
    for block in content:
        if block.get("type") == "text":
            texts.append(block["text"])
    return "\n".join(texts)


def run():
    print()
    print(f"  {C}{B}⏪ Rewind — Real LLM Test{X}")
    print(f"  {D}Proxy: {PROXY_URL}{X}")
    print(f"  {D}Model: {MODEL}{X}")
    print()

    if not API_KEY:
        print(f"  {R}Error: BEDROCK_API_KEY or ANTHROPIC_API_KEY not set{X}")
        return

    system = (
        "You are a helpful research assistant. "
        "Answer questions step by step with clear reasoning. "
        "Be concise."
    )

    # ── Step 1: Ask a question ──────────────────────────────
    print(f"  {Y}▶ Step 1:{X} Asking Claude a multi-part question...")

    messages = [
        {"role": "user", "content": (
            "I need to understand three things about Rust:\n"
            "1. What is the borrow checker?\n"
            "2. Why does Rust not have a garbage collector?\n"
            "3. What is the difference between &str and String?\n"
            "Answer each in 1-2 sentences."
        )}
    ]

    resp1 = call_bedrock(messages, system=system)
    text1 = extract_text(resp1)
    usage1 = resp1.get("usage", {})
    print(f"  {G}✓{X} Response received ({usage1.get('input_tokens', '?')}↓ {usage1.get('output_tokens', '?')}↑)")
    print()
    for line in text1.split("\n")[:10]:
        print(f"  {D}  {line}{X}")
    if text1.count("\n") > 10:
        print(f"  {D}  ...{X}")
    print()

    # ── Step 2: Follow-up question ──────────────────────────
    print(f"  {Y}▶ Step 2:{X} Follow-up question (building on context)...")

    messages.append({"role": "assistant", "content": text1})
    messages.append({"role": "user", "content": (
        "Given what you said about the borrow checker, "
        "write a short code example (under 10 lines) that would "
        "fail to compile due to a borrow checker error. "
        "Explain the error in one sentence."
    )})

    resp2 = call_bedrock(messages, system=system, max_tokens=500)
    text2 = extract_text(resp2)
    usage2 = resp2.get("usage", {})
    print(f"  {G}✓{X} Response received ({usage2.get('input_tokens', '?')}↓ {usage2.get('output_tokens', '?')}↑)")
    print()
    for line in text2.split("\n")[:15]:
        print(f"  {D}  {line}{X}")
    if text2.count("\n") > 15:
        print(f"  {D}  ...{X}")
    print()

    # ── Step 3: Challenge the answer ────────────────────────
    print(f"  {Y}▶ Step 3:{X} Challenging the answer (testing context retention)...")

    messages.append({"role": "assistant", "content": text2})
    messages.append({"role": "user", "content": (
        "Actually, can you fix that code so it compiles? "
        "Show the fixed version and explain what you changed."
    )})

    resp3 = call_bedrock(messages, system=system, max_tokens=500)
    text3 = extract_text(resp3)
    usage3 = resp3.get("usage", {})
    print(f"  {G}✓{X} Response received ({usage3.get('input_tokens', '?')}↓ {usage3.get('output_tokens', '?')}↑)")
    print()
    for line in text3.split("\n")[:15]:
        print(f"  {D}  {line}{X}")
    if text3.count("\n") > 15:
        print(f"  {D}  ...{X}")
    print()

    # ── Summary ─────────────────────────────────────────────
    total_in = sum(r.get("usage", {}).get("input_tokens", 0) for r in [resp1, resp2, resp3])
    total_out = sum(r.get("usage", {}).get("output_tokens", 0) for r in [resp1, resp2, resp3])
    print(f"  {C}{B}Done!{X} 3 LLM calls recorded by Rewind")
    print(f"  {D}Total: {total_in}↓ {total_out}↑ tokens{X}")
    print()
    print(f"  {G}Now inspect the recording:{X}")
    print(f"    {G}{B}rewind show latest{X}     {D}— see the trace{X}")
    print(f"    {G}{B}rewind inspect latest{X}  {D}— interactive TUI{X}")
    print()


if __name__ == "__main__":
    run()
