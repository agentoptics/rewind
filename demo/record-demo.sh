#!/bin/bash
set -e

export CLICOLOR_FORCE=1

REWIND="/Users/jain.r/workspace/rewind/target/release/rewind"

$REWIND demo >/dev/null 2>&1
SID=$(CLICOLOR_FORCE=0 NO_COLOR=1 $REWIND sessions 2>/dev/null | grep research-agent-demo | head -1 | awk '{print $2}')

slow_type() {
    local text="$1"
    printf "\033[32;1m❯\033[0m "
    for ((i=0; i<${#text}; i++)); do
        printf "%s" "${text:$i:1}"
        sleep 0.035
    done
    sleep 0.6
    echo
}

pause() { sleep "${1:-2}"; }

header() {
    echo ""
    echo -e "\033[90;1m# $1\033[0m"
    pause 1.5
}

clear
echo ""
echo -e "\033[1;36m⏪ Rewind — Time-Travel Debugger for AI Agents\033[0m"
echo -e "\033[90m   Fix broken agents without re-running them.\033[0m"
pause 3

header "A research agent just failed. Let's look at the trace."

slow_type "rewind show latest"
$REWIND show "$SID"
pause 4

header "Step 5 hallucinated — used stale 2019 data as current fact."
header "Rewind already forked at step 4 and replayed with the fix."
header "Let's compare the two timelines."

slow_type "rewind diff latest main fixed"
$REWIND diff "$SID" main fixed
pause 3

header "Steps 1-4 identical. Step 5 diverges: error → success."
header "Let's score both timelines with an evaluator."

slow_type "rewind eval score latest -e correctness --compare-timelines"
$REWIND eval score "$SID" -e correctness --compare-timelines
pause 3

header "Now let's check against our regression baseline."

slow_type "rewind assert check latest --against demo-baseline"
$REWIND assert check "$SID" --against demo-baseline
pause 3

header "All 37 assertions passed."
header "Let's share this debug session as a self-contained HTML file."

slow_type "rewind share latest -o rewind-demo.html"
echo "y" | $REWIND share "$SID" -o /tmp/rewind-demo.html 2>&1
pause 2

header "One HTML file — open in any browser, share via Slack or email."
header "No re-runs. No wasted tokens. The fix is proven."

echo ""
echo -e "\033[1;36m  pip install rewind-agent\033[0m"
echo -e "\033[90m  github.com/agentoptics/rewind\033[0m"
echo ""
pause 4
