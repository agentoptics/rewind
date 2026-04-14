#!/bin/bash
set -e

export CLICOLOR_FORCE=1

REWIND="/Users/jain.r/workspace/rewind/target/release/rewind"

$REWIND demo >/dev/null 2>&1

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

clear 2>/dev/null || true
echo ""
echo -e "\033[1;36m⏪ Rewind Fix — AI-Powered Diagnosis & Repair\033[0m"
echo -e "\033[90m   One command from broken to proven.\033[0m"
pause 3

header "Our research agent just failed. Let's diagnose it."

slow_type "rewind fix latest"
$REWIND fix latest
pause 4

header "Root cause identified: stale data from rate-limited API."
header "Suggested fix: retry_step (exploit LLM non-determinism)."
header "Let's apply it automatically."

slow_type "rewind fix latest --apply --yes --command \"echo 'agent replay done'\" --port 19999"
$REWIND fix latest --apply --yes --command "echo 'agent replay done'" --port 19999 2>&1 | grep -v "^2026"
pause 3

header "Fork created. Steps 1-4 cached (0 tokens). Fix verified."
header "Now let's try a hypothesis — skip diagnosis entirely."

slow_type "rewind fix latest --hypothesis swap_model:gpt-4o --apply --yes --command \"echo done\" --port 19998"
$REWIND fix latest --hypothesis "swap_model:gpt-4o" --apply --yes --command "echo done" --port 19998 2>&1 | grep -v "^2026"
pause 3

header "Model swapped from gpt-4o-mini to gpt-4o. Proxy rewrote the request."
header "No re-runs. No wasted tokens. The fix is proven."

echo ""
echo -e "\033[1;36m  pip install rewind-agent[openai]\033[0m"
echo -e "\033[90m  rewind demo && rewind fix latest\033[0m"
echo ""
pause 4
