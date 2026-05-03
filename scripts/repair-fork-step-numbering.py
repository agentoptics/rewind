#!/usr/bin/env python3
"""Renumber owned steps on replay forks created before the
`engine.fork seeds step_counters` fix landed.

Symptom: a fork with fork_at_step=N has owned steps numbered
1..M instead of N+1..N+M, so the inherited prefix is shadowed
in get_full_timeline_steps and the inherited turn-N user message
sorts AFTER the agent's response.

This script:
  * Finds every fork-timeline whose owned steps include any
    step_number <= fork_at_step.
  * Renumbers each owned step to step_number + fork_at_step
    (preserving the chronological order 1->N+1, 2->N+2, ...).
  * Updates step_counters to reflect the highest new step_number.

Safe to re-run. Idempotent — second run finds nothing to fix.

Usage:
    python3 repair-fork-step-numbering.py /data/rewind/v17/rewind.db [--apply]

Without --apply, it does a DRY RUN and prints what would change.
"""
import argparse
import sqlite3
import sys


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("db_path")
    p.add_argument("--apply", action="store_true", help="commit the changes (default: dry run)")
    args = p.parse_args()

    conn = sqlite3.connect(args.db_path)
    conn.row_factory = sqlite3.Row
    cur = conn.cursor()

    # Only target auto-generated replay forks (label `replay-*`),
    # NOT user-created forks. The latter — `edited-fork`, `fork-at-N`,
    # any custom label — may legitimately host an OWNED step at
    # step_number ≤ fork_at_step as a user EDIT that's supposed to
    # shadow the inherited row at the same position. Renumbering
    # those would break promote-and-mutate semantics.
    #
    # Replay forks always have label `replay-<8-hex>` (current naming)
    # or the legacy `replay-from-<N>` from older runners. Both start
    # with `replay-`.
    cur.execute("""
        SELECT t.id, t.label, t.fork_at_step,
               (SELECT COUNT(*) FROM steps s
                WHERE s.timeline_id = t.id AND s.step_number <= t.fork_at_step) AS broken_count
        FROM timelines t
        WHERE t.parent_timeline_id IS NOT NULL
          AND t.fork_at_step > 0
          AND t.label LIKE 'replay-%'
          AND EXISTS (
              SELECT 1 FROM steps s
              WHERE s.timeline_id = t.id AND s.step_number <= t.fork_at_step
          )
        ORDER BY t.created_at
    """)
    broken = [dict(r) for r in cur.fetchall()]

    if not broken:
        print("No broken forks found. Nothing to do.")
        return 0

    print(f"Found {len(broken)} fork(s) with broken numbering:")
    for r in broken:
        print(f"  - {r['label']} (id={r['id'][:12]}, fork_at_step={r['fork_at_step']}): "
              f"{r['broken_count']} owned step(s) need renumbering")

    if not args.apply:
        print("\nDRY RUN — re-run with --apply to commit.")

    total_renumbered = 0
    for fork in broken:
        fork_at = fork["fork_at_step"]
        # Pull all owned steps with broken numbers, ordered by their
        # current step_number so the +offset preserves chronology.
        cur.execute("""
            SELECT id, step_number FROM steps
            WHERE timeline_id = ? AND step_number <= ?
            ORDER BY step_number
        """, (fork["id"], fork_at))
        rows = cur.fetchall()
        new_max = 0
        for s in rows:
            new_num = s["step_number"] + fork_at
            new_max = max(new_max, new_num)
            print(f"    renumber step {s['id'][:12]}: {s['step_number']} -> {new_num}")
            if args.apply:
                cur.execute(
                    "UPDATE steps SET step_number = ? WHERE id = ?",
                    (new_num, s["id"]),
                )
            total_renumbered += 1

        # Sync the step_counters row so the next runner-recorded
        # step on this fork picks up where we left off, instead of
        # colliding with one of the renumbered rows.
        if args.apply and rows:
            cur.execute("""
                INSERT INTO step_counters (session_id, timeline_id, counter)
                VALUES ((SELECT session_id FROM timelines WHERE id = ?), ?, ?)
                ON CONFLICT(session_id, timeline_id) DO UPDATE
                SET counter = MAX(counter, excluded.counter)
            """, (fork["id"], fork["id"], new_max))

    if args.apply:
        conn.commit()
        print(f"\nApplied: {total_renumbered} step(s) renumbered across {len(broken)} fork(s).")
    else:
        print(f"\nWould renumber {total_renumbered} step(s) across {len(broken)} fork(s).")
    conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
