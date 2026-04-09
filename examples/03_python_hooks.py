"""
Python Hooks — Enrich proxy recordings with semantic labels.

Without hooks, steps show as "LLM Call 1", "LLM Call 2".
With hooks, they show as "search", "analyze", "summarize".

Setup:
    # Terminal 1: Start the proxy
    rewind record --name "hooks-demo" --upstream https://api.openai.com

    # Terminal 2: Run this script
    export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
    python examples/03_python_hooks.py
"""

import sys
sys.path.insert(0, "python")

import openai
import rewind_agent

# Initialize — patches OpenAI to route through proxy
rewind_agent.init()
client = openai.OpenAI()


@rewind_agent.step("search")
def search(query: str) -> str:
    """Search step — recorded as 'search' in the trace."""
    resp = client.chat.completions.create(
        model="gpt-4o",
        messages=[
            {"role": "system", "content": "You are a search engine. Return concise facts."},
            {"role": "user", "content": query},
        ],
        max_tokens=100,
    )
    return resp.choices[0].message.content


@rewind_agent.step("analyze")
def analyze(data: str) -> str:
    """Analysis step — recorded as 'analyze' in the trace."""
    resp = client.chat.completions.create(
        model="gpt-4o",
        messages=[
            {"role": "system", "content": "Analyze the following data and extract key insights."},
            {"role": "user", "content": data},
        ],
        max_tokens=150,
    )
    return resp.choices[0].message.content


# Run the agent pipeline
with rewind_agent.trace("research_pipeline"):
    data = search("What are the biggest cities in Japan?")
    print(f"Search: {data[:100]}...")

    rewind_agent.annotate("search_quality", "good")

    insights = analyze(data)
    print(f"Analysis: {insights[:100]}...")

    rewind_agent.annotate("pipeline_complete", True)

# Show all annotations
print(f"\nAnnotations: {len(rewind_agent.get_annotations())} events recorded")
print("Run 'rewind show latest' to see the enriched trace.")
