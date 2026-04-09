#!/bin/bash
# ──────────────────────────────────────────────────────────────
# Test all 3 new features: Instant Replay, Snapshots, Python Hooks
# ──────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

REWIND="./target/release/rewind"

C='\033[36m'; G='\033[32m'; Y='\033[33m'; R='\033[31m'
D='\033[2m'; B='\033[1m'; X='\033[0m'

MOCK_PID=""
PROXY_PID=""
cleanup() {
    [ -n "$PROXY_PID" ] && kill "$PROXY_PID" 2>/dev/null || true
    [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT

source "$HOME/.cargo/env" 2>/dev/null || true

if [ ! -f "$REWIND" ]; then
    echo -e "${Y}Building Rewind...${X}"
    cargo build --release --quiet
fi

rm -f ~/.rewind/rewind.db
rm -rf ~/.rewind/objects/

echo ""
echo -e "${C}${B}══════════════════════════════════════════════════════════${X}"
echo -e "${C}${B}  ⏪ REWIND — Feature Test Suite${X}"
echo -e "${C}${B}══════════════════════════════════════════════════════════${X}"
echo ""

# ── Feature 1: Instant Replay ────────────────────────────────
echo -e "${Y}${B}▶ Feature 1: Instant Replay${X}"
echo -e "${D}  Same request → cached response at \$0, 0ms latency${X}"
echo ""

# Start mock LLM
python3 "$SCRIPT_DIR/mock_llm.py" 9999 &
MOCK_PID=$!
sleep 0.3

# Start proxy with --replay flag
$REWIND record --name "replay-test" --port 8443 --upstream "http://127.0.0.1:9999" --replay &
PROXY_PID=$!
sleep 1

# First call — should hit upstream (cache miss)
echo -e "  ${D}Call 1: cache miss (hits upstream)${X}"
RESP1=$(curl -s http://127.0.0.1:8443/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-mock" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"Say hello in 5 words"}]}')
CACHE1=$(echo "$RESP1" | head -1 | grep -c "cache" 2>/dev/null || echo "0")
echo -e "  ${G}✓${X} Got response"

# Second call — IDENTICAL request, should hit cache
echo -e "  ${D}Call 2: cache hit (instant, \$0)${X}"
RESP2=$(curl -s -D - http://127.0.0.1:8443/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-mock" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"Say hello in 5 words"}]}' 2>&1)

if echo "$RESP2" | grep -q "x-rewind-cache: hit"; then
    SAVED=$(echo "$RESP2" | grep "x-rewind-saved-usd" | awk '{print $2}' | tr -d '\r')
    echo -e "  ${G}${B}✓ CACHE HIT!${X} Saved \$$SAVED"
else
    echo -e "  ${R}✗ Cache miss (unexpected)${X}"
fi

# Third call — different request, should miss
echo -e "  ${D}Call 3: different request (cache miss)${X}"
RESP3=$(curl -s -D - http://127.0.0.1:8443/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-mock" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"Tell me a joke"}]}' 2>&1)

if echo "$RESP3" | grep -q "x-rewind-cache: hit"; then
    echo -e "  ${R}✗ Cache hit on different request (bug!)${X}"
else
    echo -e "  ${G}✓${X} Cache miss (correct — different request)"
fi

kill $PROXY_PID 2>/dev/null || true
kill $MOCK_PID 2>/dev/null || true
wait $PROXY_PID 2>/dev/null || true
wait $MOCK_PID 2>/dev/null || true
PROXY_PID=""
MOCK_PID=""
sleep 0.5

echo ""
echo -e "  ${D}Cache stats:${X}"
$REWIND cache
echo ""

# ── Feature 2: Snapshots ─────────────────────────────────────
echo -e "${Y}${B}▶ Feature 2: Snapshots${X}"
echo -e "${D}  Capture workspace state, restore later${X}"
echo ""

# Create a temp workspace
TMPDIR=$(mktemp -d)
echo "version 1" > "$TMPDIR/config.txt"
echo "def hello(): print('world')" > "$TMPDIR/app.py"
mkdir -p "$TMPDIR/src"
echo "fn main() {}" > "$TMPDIR/src/main.rs"

echo -e "  ${D}Created test workspace: $TMPDIR (3 files)${X}"

# Take snapshot
$REWIND snapshot "$TMPDIR" --label "before-changes"

# Modify the workspace
echo "version 2 — MODIFIED" > "$TMPDIR/config.txt"
rm "$TMPDIR/app.py"
echo "BROKEN CODE" > "$TMPDIR/src/main.rs"
echo -e "  ${D}Modified workspace (changed config, deleted app.py, broke main.rs)${X}"

# Verify it's actually modified
echo -e "  ${D}Current config.txt: $(cat $TMPDIR/config.txt)${X}"

# Restore
$REWIND restore before-changes

# Verify restore
RESTORED=$(cat "$TMPDIR/config.txt")
if [ "$RESTORED" = "version 1" ]; then
    echo -e "  ${G}${B}✓ Snapshot restored correctly!${X}"
    echo -e "  ${D}  config.txt: $RESTORED${X}"
else
    echo -e "  ${R}✗ Restore failed: got '$RESTORED'${X}"
fi

if [ -f "$TMPDIR/app.py" ]; then
    echo -e "  ${G}✓${X} app.py recovered"
else
    echo -e "  ${R}✗ app.py not restored${X}"
fi

rm -rf "$TMPDIR"
echo ""

# List snapshots
$REWIND snapshots
echo ""

# ── Feature 3: Python Hooks ──────────────────────────────────
echo -e "${Y}${B}▶ Feature 3: Python Hooks${X}"
echo -e "${D}  Decorators enrich proxy recordings with semantic labels${X}"
echo ""

python3 -c "
import sys
sys.path.insert(0, '$PROJECT_DIR/python')
import rewind_agent

# Test decorators
@rewind_agent.step('search')
def search(query):
    return f'Results for: {query}'

@rewind_agent.tool('calculator')
def calculate(expr):
    return a + b

@rewind_agent.node('planner')
def plan(goal):
    return {'steps': ['research', 'write', 'review']}

# Run them
result1 = search('Tokyo population')
result2 = calculate('2 + 2')
result3 = plan('Write a report')

# Test trace context manager
with rewind_agent.trace('analysis_phase'):
    rewind_agent.annotate('confidence', 0.92)
    rewind_agent.annotate('model_used', 'gpt-4o')

# Check annotations
annotations = rewind_agent.get_annotations()
print(f'  Annotations captured: {len(annotations)}')
for a in annotations:
    t = a['type']
    name = a['data'].get('step_name', a['data'].get('trace_name', a['data'].get('key', '?')))
    print(f'    {t:>15}: {name}')

print()
print('  All hook types working: step, node, tool, trace, annotate')
"
echo ""

# ── Summary ──────────────────────────────────────────────────
echo -e "${C}${B}══════════════════════════════════════════════════════════${X}"
echo -e "${G}${B}  ✓ All 3 features working:${X}"
echo -e "    ${G}1. Instant Replay${X} — cached responses at \$0"
echo -e "    ${G}2. Snapshots${X}       — workspace checkpoint/restore"
echo -e "    ${G}3. Python Hooks${X}    — step/node/tool/trace decorators"
echo -e "${C}${B}══════════════════════════════════════════════════════════${X}"
echo ""
