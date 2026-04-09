#!/usr/bin/env python3
"""
Generate an asciinema .cast file with hand-crafted timing.
No TTY needed — writes the file directly.
"""

import json
import os
import subprocess
import sys

# Run the rewind show command to get fresh output
rewind_bin = os.path.join(os.path.dirname(__file__), "..", "target", "release", "rewind")
rewind_output = subprocess.run(
    [rewind_bin, "show", "latest"],
    capture_output=True, text=True
).stdout

COLS = 92
ROWS = 38

# ── Build frames: (timestamp_seconds, text_to_output) ─────────

frames = []
t = 0.0


def emit(text, dt=0.0):
    global t
    t += dt
    frames.append((t, text))


def type_text(text, char_delay=0.04, pre_delay=0.0):
    global t
    t += pre_delay
    for ch in text:
        frames.append((t, ch))
        t += char_delay
    frames.append((t, "\r\n"))


def print_line(text, dt=0.02):
    emit(text + "\r\n", dt)


def pause(seconds):
    global t
    t += seconds


# ── Scene 1: Title ────────────────────────────────────────────

pause(0.5)
print_line("")
print_line("  \033[36m\033[1m<< Rewind Demo — Direct Recording\033[0m", 0.05)
print_line("  \033[2mOne line to record. One command to debug.\033[0m", 0.05)
print_line("")
pause(2.0)

# ── Scene 2: Python REPL — init() ────────────────────────────

print_line("  \033[32m\033[1m$ python3\033[0m", 0.05)
pause(0.5)

emit("  \033[35m>>>\033[0m ")
type_text("import rewind_agent", char_delay=0.05)
pause(0.3)

emit("  \033[35m>>>\033[0m ")
type_text("rewind_agent.init()", char_delay=0.05)
pause(0.3)

# Banner (matches the new _print_direct_banner)
print_line("")
print_line("  \033[36m\033[1m  <<  r e w i n d\033[0m", 0.02)
print_line("  \033[2m  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\033[0m", 0.02)
print_line("  \033[2m  The time-travel debugger for AI agents\033[0m", 0.02)
print_line("")
print_line("  \033[36m\033[1mRecording active\033[0m \033[90m(direct)\033[0m", 0.02)
print_line("")
print_line("  \033[90m  Session:\033[0m  default", 0.02)
print_line("  \033[90m  Store:\033[0m    ~/.rewind/", 0.02)
print_line("  \033[90m  Debug:\033[0m    \033[32mrewind show latest\033[0m", 0.02)
print_line("")
print_line("  \033[33m  ● Recording all LLM calls\033[0m", 0.02)
print_line("")
pause(2.0)

# ── Scene 3: Agent runs ──────────────────────────────────────

emit("  \033[35m>>>\033[0m ")
type_text("result = my_agent.run('Research Tokyo population')", char_delay=0.04)
pause(0.5)

print_line("  \033[2m  🧠 Step 1: LLM → tool_calls: web_search\033[0m", 0.3)
pause(0.5)
print_line("  \033[2m  📋 Step 2: Tool result — fresh 2024 data\033[0m", 0.3)
pause(0.5)
print_line("  \033[31m  ⚠ Step 3: Tool result — STALE cached data from 2019!\033[0m", 0.3)
pause(1.0)
print_line("")
print_line("  \033[31m\033[1m  ✗ Agent hallucinated — used stale 2019 data as fact\033[0m", 0.05)
pause(2.5)

# ── Scene 4: rewind show ─────────────────────────────────────

print_line("")
emit("  \033[32m\033[1m$ \033[0m")
type_text("rewind show latest", char_delay=0.05)
pause(0.5)

# Output the actual rewind show output
for line in rewind_output.split("\n"):
    print_line(line, 0.04)
pause(4.0)

# ── Scene 5: Closing pitch ───────────────────────────────────

print_line("")
print_line("  \033[36m\033[1m━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\033[0m", 0.02)
print_line("")
print_line("  \033[36mOne line to record:\033[0m  \033[32mrewind_agent.init()\033[0m", 0.05)
print_line("  \033[36mOne command to debug:\033[0m \033[32mrewind show latest\033[0m", 0.05)
print_line("")
print_line("  \033[33mpip install rewind-agent\033[0m", 0.05)
print_line("  \033[2mgithub.com/agentoptics/rewind\033[0m", 0.05)
print_line("")
print_line("  \033[36m\033[1m━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\033[0m", 0.02)
print_line("")
pause(3.0)

# ── Write .cast file ──────────────────────────────────────────

cast_path = os.path.join(os.path.dirname(__file__), "demo-direct.cast")

with open(cast_path, "w") as f:
    # Header
    header = {
        "version": 2,
        "width": COLS,
        "height": ROWS,
        "timestamp": 1712000000,
        "env": {"TERM": "xterm-256color", "SHELL": "/bin/zsh"},
    }
    f.write(json.dumps(header) + "\n")

    # Frames
    for ts, text in frames:
        f.write(json.dumps([round(ts, 6), "o", text]) + "\n")

print(f"Generated {cast_path} ({len(frames)} frames, {t:.1f}s)")
