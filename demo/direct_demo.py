#!/usr/bin/env python3
"""
Demo: Direct recording — one line to record, no proxy needed.

This script simulates what a real user experience looks like:
1. import rewind_agent; rewind_agent.init()
2. Agent makes LLM calls (simulated here with direct store writes)
3. Agent fails — hallucination detected
4. User runs `rewind show latest` to see the trace

For the demo GIF, we print the Python code as if it's being typed,
then show the actual recorded output.
"""

import sys
import time
import os

# Add the python package to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))

from rewind_agent.store import Store
from rewind_agent.recorder import Recorder

# Colors
C = "\033[36m"   # cyan
G = "\033[32m"   # green
Y = "\033[33m"   # yellow
R = "\033[31m"   # red
D = "\033[2m"    # dim
B = "\033[1m"    # bold
M = "\033[35m"   # magenta
X = "\033[0m"    # reset


def typewrite(text, delay=0.03):
    """Simulate typing."""
    for ch in text:
        sys.stdout.write(ch)
        sys.stdout.flush()
        time.sleep(delay)
    print()


def slow_print(text, delay=0.01):
    """Print with slight delay per character."""
    for ch in text:
        sys.stdout.write(ch)
        sys.stdout.flush()
        time.sleep(delay)
    print()


def section_pause(seconds=1.5):
    time.sleep(seconds)


def main():
    # ── Part 1: Show the one-liner ──────────────────────────────
    print()
    print(f"  {C}{B}⏪ Rewind Demo — Direct Recording{X}")
    print(f"  {D}One line to record. One command to debug.{X}")
    print()
    section_pause(2)

    # Simulate Python REPL
    print(f"  {G}{B}$ python3{X}")
    section_pause(0.5)
    print(f"  {M}>>>{X} ", end="")
    typewrite("import rewind_agent", 0.04)
    section_pause(0.3)
    print(f"  {M}>>>{X} ", end="")
    typewrite("rewind_agent.init()", 0.04)

    # Print the banner (simulated to match real output)
    section_pause(0.3)
    print()
    print(f"  {C}{B}⏪ Rewind{X} — Recording active (direct)")
    print()
    print(f"  {D}Session:{X} default")
    print(f"  {D}Store:{X}   ~/.rewind/")
    print()
    print(f"  {Y}All LLM calls are being recorded.{X}")
    print()
    section_pause(2)

    # ── Part 2: Agent runs and fails ────────────────────────────
    print(f"  {M}>>>{X} ", end="")
    typewrite("# Run your agent — every LLM call is captured", 0.03)
    print(f"  {M}>>>{X} ", end="")
    typewrite("result = my_agent.run('Research Tokyo population')", 0.04)
    section_pause(0.5)

    # Actually create real recorded data
    store = Store()
    sid, tid = store.create_session("research-agent")
    recorder = Recorder(store, sid, tid)

    # Step 1: LLM call with tool use
    section_pause(0.5)
    print(f"  {D}  🧠 Step 1: LLM → tool_calls: web_search{X}")
    recorder._record_call(
        model="gpt-4o",
        request_data={
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are a research assistant. Use tools to find information."},
                {"role": "user", "content": "What is the current population of Tokyo?"},
            ],
            "tools": [{"type": "function", "function": {"name": "web_search"}}],
        },
        response_data={
            "choices": [{"message": {"role": "assistant", "content": None, "tool_calls": [
                {"id": "call_1", "function": {"name": "web_search", "arguments": '{"query": "Tokyo population 2024"}'}}
            ]}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 156, "completion_tokens": 28},
        },
        duration_ms=320,
        provider="openai",
    )
    time.sleep(0.3)

    # Step 2: Tool result (good data)
    print(f"  {D}  📋 Step 2: Tool result — fresh 2024 data{X}")
    recorder._record_call(
        model="gpt-4o",
        request_data={
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are a research assistant."},
                {"role": "user", "content": "What is the current population of Tokyo?"},
                {"role": "assistant", "tool_calls": [{"id": "call_1", "function": {"name": "web_search"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "Tokyo 23 wards: 13.96M (2024). Peaked at 14.04M in 2020. Declined due to COVID."},
            ],
            "tools": [{"type": "function", "function": {"name": "web_search"}}],
        },
        response_data={
            "choices": [{"message": {"role": "assistant", "content": None, "tool_calls": [
                {"id": "call_2", "function": {"name": "web_search", "arguments": '{"query": "Tokyo population trend decade"}'}}
            ]}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 312, "completion_tokens": 35},
        },
        duration_ms=890,
        provider="openai",
    )
    time.sleep(0.3)

    # Step 3: Tool result (STALE data!)
    print(f"  {R}  ⚠ Step 3: Tool result — STALE cached data from 2019!{X}")
    recorder._record_call(
        model="gpt-4o",
        request_data={
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are a research assistant."},
                {"role": "user", "content": "What is the current population of Tokyo?"},
                {"role": "tool", "tool_call_id": "call_2",
                 "content": "ERROR: Rate limited. Cached 2019 data: Tokyo projected to reach 14.2M by 2025. Steady growth, no disruptions expected."},
            ],
        },
        response_data={
            "choices": [{"message": {"role": "assistant",
                "content": "# Tokyo Population\n\nTokyo currently has **14.2 million** people (2024). "
                "The city has seen **steady, uninterrupted growth** over the past decade, "
                "rising from 13.35M in 2014 to 14.2M today.\n\n"
                "No significant disruptions have been observed during this period."},
                "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 520, "completion_tokens": 180},
        },
        duration_ms=1450,
        provider="openai",
        error="HALLUCINATION: Used stale 2019 projection (14.2M) as fact. Ignored COVID-19 dip. Actual: 13.96M.",
    )
    time.sleep(0.3)

    store.update_session_status(sid, "failed")
    store.close()

    print()
    print(f"  {R}{B}  ✗ Agent hallucinated — used stale 2019 data as fact{X}")
    section_pause(2.5)

    # ── Part 3: Debug with rewind show ──────────────────────────
    print()
    print(f"  {G}{B}$ rewind show latest{X}")
    sys.stdout.flush()
    section_pause(0.5)

    # Run the actual rewind binary
    rewind = os.path.join(os.path.dirname(__file__), "..", "target", "release", "rewind")
    sys.stdout.flush()
    os.system(f"{rewind} show latest")
    section_pause(4)

    # ── Part 4: The pitch ───────────────────────────────────────
    print()
    print(f"  {C}{B}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{X}")
    print()
    print(f"  {C}One line to record:{X}  {G}rewind_agent.init(){X}")
    print(f"  {C}One command to debug:{X} {G}rewind show latest{X}")
    print()
    print(f"  {Y}pip install rewind-agent{X}")
    print(f"  {D}github.com/agentoptics/rewind{X}")
    print()
    print(f"  {C}{B}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{X}")
    print()
    section_pause(3)


if __name__ == "__main__":
    main()
