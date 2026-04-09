#!/usr/bin/env python3
"""
Demo research agent that runs through the Rewind proxy.

This simulates a multi-step tool-calling agent:
  1. Gets asked about Tokyo population
  2. Calls web_search tool (simulated)
  3. Receives search results (first one is good, second is STALE)
  4. Synthesizes an answer — but hallucinates due to stale data

The agent talks to whatever OPENAI_BASE_URL is set to.
When Rewind proxy is running, all calls get recorded automatically.
"""

import json
import os
import sys
import urllib.request

API_BASE = os.environ.get("OPENAI_BASE_URL", "http://127.0.0.1:8443/v1")
API_KEY = os.environ.get("OPENAI_API_KEY", "sk-mock-key")

# Colors
C = "\033[36m"
G = "\033[32m"
Y = "\033[33m"
R = "\033[31m"
D = "\033[2m"
B = "\033[1m"
X = "\033[0m"

# Simulated tool responses
TOOL_RESPONSES = {
    "call_search_1": (
        "Tokyo metropolitan area population (2024): approximately 13.96 million "
        "in the 23 special wards, 37.4 million in the Greater Tokyo Area. "
        "The population of the 23 wards peaked in 2020 at 14.04 million "
        "before a slight decline attributed to COVID-19 migration patterns. "
        "Source: Tokyo Metropolitan Government Statistics Bureau."
    ),
    "call_search_2": (
        "ERROR: Search API rate limited. Cached result returned from 2019 dataset. "
        "Tokyo population trend 2014-2019: steady growth from 13.35M to 13.96M "
        "in 23 wards (+4.6%). National Institute of Population projections (2019): "
        "expected continued growth through 2025, reaching 14.2M. "
        "Note: this data predates COVID-19 impacts."
    ),
}


def call_llm(messages: list, tools: list | None = None) -> dict:
    """Make a chat completion call to the API (through Rewind proxy)."""
    payload = {
        "model": "gpt-4o",
        "messages": messages,
    }
    if tools:
        payload["tools"] = tools

    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{API_BASE}/chat/completions",
        data=data,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
    )

    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def run_agent():
    print()
    print(f"  {C}{B}🤖 Research Agent{X} — starting")
    print(f"  {D}API: {API_BASE}{X}")
    print()

    tools = [{
        "type": "function",
        "function": {
            "name": "web_search",
            "description": "Search the web for current information",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": "Search query"}},
                "required": ["query"]
            }
        }
    }]

    messages = [
        {"role": "system", "content": (
            "You are a research assistant. When asked about a topic, use the "
            "provided tools to search for information and synthesize an "
            "accurate answer with citations."
        )},
        {"role": "user", "content": (
            "What is the current population of Tokyo, and how has it "
            "changed over the last decade?"
        )},
    ]

    step = 0
    max_steps = 10  # safety limit

    while step < max_steps:
        step += 1
        print(f"  {Y}▶ Step {step}:{X} Calling LLM...", end=" ", flush=True)

        response = call_llm(messages, tools)
        choice = response["choices"][0]
        message = choice["message"]
        finish_reason = choice["finish_reason"]
        usage = response.get("usage", {})

        tokens_in = usage.get("prompt_tokens", 0)
        tokens_out = usage.get("completion_tokens", 0)

        if finish_reason == "tool_calls" and message.get("tool_calls"):
            # Agent wants to call tools
            tool_calls = message["tool_calls"]
            tool_names = [tc["function"]["name"] for tc in tool_calls]
            print(f"{G}tool_calls: {', '.join(tool_names)}{X} ({tokens_in}↓ {tokens_out}↑)")

            # Add assistant message to history
            messages.append(message)

            # Process each tool call
            for tc in tool_calls:
                tc_id = tc["id"]
                func_name = tc["function"]["name"]
                func_args = json.loads(tc["function"]["arguments"])

                # Look up simulated response
                tool_result = TOOL_RESPONSES.get(tc_id, f"No result for {func_name}({func_args})")

                print(f"  {D}  🔧 {func_name}({func_args.get('query', '...')}){X}")

                if "ERROR" in tool_result:
                    print(f"  {R}  ⚠ Stale cached data returned!{X}")

                messages.append({
                    "role": "tool",
                    "tool_call_id": tc_id,
                    "content": tool_result,
                })

        elif message.get("content"):
            # Agent produced a final answer
            content = message["content"]
            print(f"{G}response{X} ({tokens_in}↓ {tokens_out}↑)")
            print()

            # Check for hallucination markers
            has_error = False
            if "14.2 million" in content and "2024" in content:
                has_error = True
            if "no significant disruptions" in content.lower() or "uninterrupted growth" in content.lower():
                has_error = True

            # Print the response
            for line in content.split("\n"):
                if has_error and ("14.2" in line or "uninterrupted" in line.lower() or "no significant" in line.lower()):
                    print(f"  {R}{B}  {line}{X}  {R}← WRONG{X}")
                else:
                    print(f"  {D}  {line}{X}")

            print()
            if has_error:
                print(f"  {R}{B}✗ Agent hallucinated!{X}")
                print(f"  {R}  Used stale 2019 projection (14.2M) as 2024 fact.{X}")
                print(f"  {R}  Claimed 'no disruptions' despite COVID-19 data in context.{X}")
                print()
                print(f"  {C}This is exactly the kind of bug Rewind catches.{X}")
                print(f"  {C}Run: {G}{B}rewind show latest{X}{C} to see the trace.{X}")
                print(f"  {C}Run: {G}{B}rewind inspect latest{X}{C} to explore interactively.{X}")
            else:
                print(f"  {G}{B}✓ Agent responded correctly.{X}")

            print()
            return has_error

        else:
            print(f"{R}unexpected response{X}")
            break

    return True


if __name__ == "__main__":
    sys.exit(1 if run_agent() else 0)
