#!/bin/bash
# This script is recorded by asciinema to produce the demo GIF.
# It runs non-interactively — no TUI (can't record interactive TUI via script).

REWIND="$HOME/workspace/rewind/target/release/rewind"

sleep 0.5

# Show the trace
echo "$ rewind show latest"
sleep 0.3
$REWIND show latest
sleep 2

# Show the diff
echo ""
echo "$ rewind diff latest main fixed"
sleep 0.3
$REWIND diff latest main fixed
sleep 2

# Show instant replay cache
echo ""
echo "$ rewind cache"
sleep 0.3
$REWIND cache
sleep 1.5

# Show snapshots
echo ""
echo "$ rewind snapshots"
sleep 0.3
$REWIND snapshots 2>/dev/null || echo "  (no snapshots yet — use: rewind snapshot . --label checkpoint)"
sleep 1.5

# Show sessions
echo ""
echo "$ rewind sessions"
sleep 0.3
$REWIND sessions
sleep 1.5
