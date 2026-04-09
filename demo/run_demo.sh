#!/bin/bash
# ──────────────────────────────────────────────────────────────
# ⏪ Rewind Demo — See it in action
#
# This script demonstrates the full Rewind workflow:
# 1. Seed a realistic agent failure scenario
# 2. Show the session trace (the "before" — agent fails)
# 3. Inspect with TUI
# 4. Show the diff (the "after" — fork fixes it)
# ──────────────────────────────────────────────────────────────

set -e

# Colors
CYAN='\033[36m'
GREEN='\033[32m'
YELLOW='\033[33m'
RED='\033[31m'
DIM='\033[2m'
BOLD='\033[1m'
RESET='\033[0m'

REWIND="cargo run --quiet --"

echo ""
echo -e "${CYAN}${BOLD}════════════════════════════════════════════════════════════${RESET}"
echo -e "${CYAN}${BOLD}  ⏪ REWIND — Time-Travel Debugger for AI Agents${RESET}"
echo -e "${CYAN}${BOLD}════════════════════════════════════════════════════════════${RESET}"
echo ""
echo -e "${DIM}  Chrome DevTools for AI agents — checkpoint, replay, intervene, diff.${RESET}"
echo ""

# ── Step 1: Seed demo data ────────────────────────────────────
echo -e "${YELLOW}${BOLD}▶ Step 1: Seeding a realistic agent scenario...${RESET}"
echo -e "${DIM}  A research agent searches for Tokyo population data.${RESET}"
echo -e "${DIM}  It gets a stale cached result and hallucinates on the final step.${RESET}"
echo ""
$REWIND demo
echo ""

# ── Step 2: Show the failure ──────────────────────────────────
echo -e "${YELLOW}${BOLD}▶ Step 2: The agent trace — see where it went wrong${RESET}"
echo ""
$REWIND show latest
echo ""

echo -e "${RED}${BOLD}  ⚠ The agent failed on Step 5!${RESET}"
echo -e "${DIM}  It used a stale 2019 projection as current fact and${RESET}"
echo -e "${DIM}  claimed \"no significant disruptions\" despite COVID-19 data.${RESET}"
echo ""
echo -e "${DIM}  Without Rewind, you'd have to:${RESET}"
echo -e "${DIM}    1. Re-run all 5 steps (\$0.0049, ~3 seconds)${RESET}"
echo -e "${DIM}    2. Hope the search API doesn't return stale data again${RESET}"
echo -e "${DIM}    3. Pray the LLM doesn't hallucinate the same way${RESET}"
echo ""

# ── Step 3: Show the diff ─────────────────────────────────────
echo -e "${YELLOW}${BOLD}▶ Step 3: With Rewind — fork at Step 4, fix the context, diff${RESET}"
echo -e "${DIM}  We forked at Step 4, corrected the stale tool response,${RESET}"
echo -e "${DIM}  and re-ran only Step 5. Cost: \$0.0032 for 1 LLM call.${RESET}"
echo ""
$REWIND diff latest main fixed
echo ""

echo -e "${GREEN}${BOLD}  ✓ The fork produces the correct answer!${RESET}"
echo -e "${DIM}  Steps 1-4 are shared (zero cost). Only Step 5 was re-run.${RESET}"
echo -e "${GREEN}  Saved: ${BOLD}\\$0.0017${RESET}${GREEN} and ~2 seconds of LLM calls.${RESET}"
echo ""

# ── Step 4: Interactive TUI ───────────────────────────────────
echo -e "${YELLOW}${BOLD}▶ Step 4: Explore interactively${RESET}"
echo ""
echo -e "  Run: ${GREEN}${BOLD}rewind inspect latest${RESET}"
echo -e "  ${DIM}Navigate steps with ↑↓, Tab to switch panels, q to quit.${RESET}"
echo -e "  ${DIM}See the exact context window at each step — what the LLM saw.${RESET}"
echo ""
echo -e "${CYAN}${BOLD}════════════════════════════════════════════════════════════${RESET}"
echo -e "${CYAN}  That's Rewind. Your agent failed on step 5.${RESET}"
echo -e "${CYAN}  You had two options:${RESET}"
echo ""
echo -e "${RED}  A) Re-run all 5 steps. Pay full cost. Wait. Hope.${RESET}"
echo -e "${GREEN}  B) rewind fork --at 4 → fix context → 1 LLM call → done.${RESET}"
echo ""
echo -e "${CYAN}${BOLD}════════════════════════════════════════════════════════════${RESET}"
echo ""
