#!/bin/bash
# ──────────────────────────────────────────────────────────────
# ⏪ Rewind — Full End-to-End Demo
#
# This runs the COMPLETE flow:
#   1. Start a mock LLM server (simulates OpenAI API)
#   2. Start the Rewind proxy (intercepts & records all calls)
#   3. Run a Python agent that makes multi-step tool calls
#   4. Agent FAILS — hallucinates due to stale cached data
#   5. Use Rewind CLI to diagnose exactly what went wrong
#   6. Show the forked timeline with the correct answer
#
# No API keys needed. Everything runs locally.
# ──────────────────────────────────────────────────────────────

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

# Colors
C='\033[36m'; G='\033[32m'; Y='\033[33m'; R='\033[31m'
D='\033[2m'; B='\033[1m'; X='\033[0m'

REWIND="./target/release/rewind"

# Ensure release binary exists
if [ ! -f "$REWIND" ]; then
    echo -e "${Y}Building Rewind (release)...${X}"
    source "$HOME/.cargo/env" 2>/dev/null || true
    cargo build --release --quiet
fi

# Clean previous data
rm -f ~/.rewind/rewind.db
rm -rf ~/.rewind/objects/

# Pids to clean up
MOCK_PID=""
PROXY_PID=""
cleanup() {
    [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true
    [ -n "$PROXY_PID" ] && kill "$PROXY_PID" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT

echo ""
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo -e "${C}${B}  ⏪ REWIND — End-to-End Demo${X}"
echo -e "${C}${B}  Chrome DevTools for AI agents: checkpoint, replay, intervene, diff${X}"
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo ""

# ── Phase 1: Start infrastructure ────────────────────────────
echo -e "${Y}${B}Phase 1: Starting infrastructure${X}"
echo ""

# Start mock LLM
python3 "$SCRIPT_DIR/mock_llm.py" 9999 &
MOCK_PID=$!
sleep 0.5

# Start Rewind proxy (forwarding to mock LLM)
echo -e "  ${D}Starting Rewind proxy on :8443 → mock LLM on :9999${X}"
$REWIND record --name "research-agent-live" --port 8443 --upstream "http://127.0.0.1:9999" &
PROXY_PID=$!
sleep 1

echo -e "  ${G}✓${X} Mock LLM running (pid $MOCK_PID)"
echo -e "  ${G}✓${X} Rewind proxy running (pid $PROXY_PID)"
echo ""

# ── Phase 2: Run the agent ───────────────────────────────────
echo -e "${Y}${B}Phase 2: Running the research agent${X}"
echo -e "${D}  The agent will search for Tokyo population data.${X}"
echo -e "${D}  It will receive stale cached data and hallucinate.${X}"
echo ""

export OPENAI_BASE_URL="http://127.0.0.1:8443/v1"
export OPENAI_API_KEY="sk-mock-key"

python3 "$SCRIPT_DIR/agent.py" || true

# Stop proxy and mock (we have our recording)
kill "$PROXY_PID" 2>/dev/null || true
kill "$MOCK_PID" 2>/dev/null || true
PROXY_PID=""
MOCK_PID=""
sleep 0.5

# ── Phase 3: Diagnose with Rewind ────────────────────────────
echo ""
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo -e "${Y}${B}Phase 3: Diagnosing with Rewind${X}"
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo ""

echo -e "${D}  The agent's LLM calls were recorded by the Rewind proxy.${X}"
echo -e "${D}  Let's see exactly what happened:${X}"
echo ""

$REWIND sessions
echo ""

echo -e "${Y}${B}─── Full Trace ───${X}"
echo ""
$REWIND show latest
echo ""

# ── Phase 4: What Rewind reveals ─────────────────────────────
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo -e "${Y}${B}Phase 4: What Rewind reveals${X}"
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo ""
echo -e "  ${C}With Rewind, you can see the ${B}exact context window${X}${C} at each step.${X}"
echo ""
echo -e "  ${D}At Step 3 (the final LLM call), the context contained:${X}"
echo -e "  ${D}  - A ${G}good${X}${D} search result: 'population peaked at 14.04M in 2020,${X}"
echo -e "  ${D}    declined to 13.96M due to COVID-19'${X}"
echo -e "  ${D}  - A ${R}stale${X}${D} search result: 'ERROR: cached from 2019...${X}"
echo -e "  ${D}    projected growth to 14.2M...predates COVID-19'${X}"
echo ""
echo -e "  ${R}The LLM picked the stale projection (14.2M) over the actual data (13.96M)${X}"
echo -e "  ${R}and claimed 'no significant disruptions' — ignoring COVID-19 entirely.${X}"
echo ""
echo -e "  ${C}${B}Without Rewind:${X} Re-run all steps. Pay again. Hope for better luck."
echo -e "  ${G}${B}With Rewind:${X} Fork at the stale tool result, fix it, re-run only 1 step."
echo ""
echo -e "  ${D}Try it yourself:${X}"
echo -e "    ${G}rewind inspect latest${X}   ${D}— interactive TUI, see every step${X}"
echo ""
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo -e "${C}  ⏪ That's Rewind. Time-travel debugging for AI agents.${X}"
echo -e "${C}${B}══════════════════════════════════════════════════════════════════${X}"
echo ""
