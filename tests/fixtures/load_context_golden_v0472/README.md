# load_context golden outputs (handoff-mcp v0.47.2)

`handoff_load_context` output captured from the real v0.47.2 binary
(`handoff-mcp session load`), used by v0.47.3 WS2 to check that
`task_tree: "full"` keeps `task_tree` and `task_summary` unchanged.

| file | project |
|---|---|
| `small.json` | 29 tasks: parent/child/grandchild, every status (todo/in_progress/blocked/review/done/skipped), dependencies, one closed previous session |
| `todo300.json` | 300 tasks (299 todo + 1 in_progress parent with 240 children), one closed previous session whose `related_task_ids` has 299 IDs |

All data is synthetic. No real `.handoff` data is included.

## Regenerate

```
scripts/gen-load-context-golden-v0472.sh [/tmp/golden-v0472/handoff-mcp-v0.47.2]
```

The projects are built in a temp dir through the binary's CLI only. Due dates are
fixed (`2020-01-10` is always overdue, `2099-12-31` never), so `overdue_count`
does not depend on the run date.

## Volatile values

`scripts/normalize-load-context-golden.py` replaces, keeping the keys:
`agent_id` -> `<agent_id>`, `previous_session.id` -> `<session_id>`,
`ended_at`/`started_at`/`created_at`/`updated_at`/`completed_at`/`timestamp` and any
ISO-8601 string -> `<ts>`, `s-YYYYMMDD-HHMMSS-N` / `a-...` IDs inside strings.
`branch`/`commit` are `"unknown"` because the temp dir is not a git repo.

## Comparing

Compare **semantically** (parse JSON, order-independent object comparison).
Do not compare bytes: `task_summary.by_status` is a `HashMap` in v0.47.2 and its key
order varies from run to run (WS2 will change it to a `BTreeMap`). The key order
recorded here is just whatever that run produced. Compare arrays (`task_tree`,
`related_task_ids`) in order.

When comparing against a newer binary, normalize its output with the same script
first. Only `task_tree` and `task_summary` are expected to be identical under
`task_tree: "full"`; other keys may legitimately change in v0.47.3.
