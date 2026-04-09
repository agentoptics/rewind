"""
LangGraph Integration — Auto-instrument all graph nodes.

Setup:
    pip install langgraph openai

    # Terminal 1: Start the proxy
    rewind record --name "langgraph-demo" --upstream https://api.openai.com

    # Terminal 2: Run this script
    export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
    python examples/04_langgraph.py
"""

import sys
sys.path.insert(0, "python")

try:
    from langgraph.graph import StateGraph, END
    from typing import TypedDict
except ImportError:
    print("This example requires langgraph: pip install langgraph")
    sys.exit(1)

import openai
import rewind_agent

rewind_agent.init()
client = openai.OpenAI()


# Define graph state
class AgentState(TypedDict):
    query: str
    research: str
    answer: str


# Define nodes
@rewind_agent.node("researcher")
def research(state: AgentState) -> dict:
    resp = client.chat.completions.create(
        model="gpt-4o",
        messages=[
            {"role": "system", "content": "Research the topic and provide key facts."},
            {"role": "user", "content": state["query"]},
        ],
        max_tokens=200,
    )
    return {"research": resp.choices[0].message.content}


@rewind_agent.node("writer")
def write_answer(state: AgentState) -> dict:
    resp = client.chat.completions.create(
        model="gpt-4o",
        messages=[
            {"role": "system", "content": "Write a concise answer based on the research."},
            {"role": "user", "content": f"Research: {state['research']}\n\nQuestion: {state['query']}"},
        ],
        max_tokens=200,
    )
    return {"answer": resp.choices[0].message.content}


# Build graph
builder = StateGraph(AgentState)
builder.add_node("researcher", research)
builder.add_node("writer", write_answer)
builder.set_entry_point("researcher")
builder.add_edge("researcher", "writer")
builder.add_edge("writer", END)

graph = builder.compile()

# Run
result = graph.invoke({"query": "What is Rust's borrow checker?", "research": "", "answer": ""})
print(f"Answer: {result['answer'][:200]}...")
print("\nRun 'rewind show latest' to see the graph execution trace.")
