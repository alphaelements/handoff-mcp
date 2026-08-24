---
description: "Multi-WT extension for session-loop — read this when config.toml [worktree.session_loop] auto_assign = true"
---

# Multi-WT Session Loop Extension

This file contains the multi-worktree steps referenced by `session-loop.md`.
**Read this only when `auto_assign = true` in `config.toml`.** When `auto_assign`
is `false` (default) or absent, these steps do not apply.

These steps interleave with the main session-loop procedure:
- **Steps 1b–1d** run between Step 1 (fetch tasks) and Step 2 (plan implementation)
- **Step 5-wt** replaces the conventional Step 5 (launch Workflow)
- **Step 6 multi-WT** replaces the conventional Step 6 (process results)
- **Steps 9–10** run after Step 8 (next session), before session close

## Step 1b. Functional grouping + WT assignment plan

**Gate check:**

```
handoff_get_config
```

Read `worktree.session_loop.auto_assign`. If it is absent or `false`, skip
Steps 1b/1c/1d and continue at Step 2 with the todo tasks fetched in Step 1.
Only proceed with the rest of this step when it is explicitly `true`.

**Grouping logic** (applied to the todo tasks fetched in Step 1):

1. **Dependency chains**: tasks connected by `dependencies` belong to the same
   group — a WT boundary must never split a dependency edge.
2. **File scope overlap**: fetch each task's `scope_paths` via `handoff_get_task`.
   Tasks whose `scope_paths` overlap (same file/dir, or a prefix relationship)
   belong to the same group. When `scope_paths` is empty for a task, estimate
   its file footprint from `notes`/`title` instead of leaving it ungrouped.
3. **Functional proximity**: tasks that clearly belong to the same feature or
   subsystem (e.g. all "auth", all "api") are grouped together even without a
   direct dependency or scope overlap — this is a manager (LLM) judgment call,
   not a mechanical rule.
4. **Single-group fallback**: if grouping produces a single group, or the total
   task count is `<= MAX_TASKS_PER_SESSION`, **do not use WTs** — fall back to
   the conventional single-session flow (Step 2 onward) exactly as if
   `auto_assign` were `false`. Multi-WT execution only makes sense when there
   are genuinely independent, parallelizable groups.

**Present the plan to the user** in this format before doing anything else:

```
### WT assignment plan

| Group         | Tasks         | WT  | Branch     |
|----------------|--------------|-----|------------|
| feature-auth   | t50, t51, t52 | wt2 | feat/auth  |
| feature-api    | t56, t57      | wt3 | feat/api   |

Merge strategy: merge-commit (from config.toml)

Approve?
```

- **Group**: a short slug describing the functional area (used to derive the
  branch name).
- **Tasks**: the task IDs assigned to that group, in the same ID format used
  elsewhere in this document (comma-separated, bundled IDs allowed).
- **WT**: the worktree label the group will run in (`wt2`, `wt3`, ... — the
  primary worktree running this manager is never reassigned).
- **Branch**: the branch that WT will be created on or reused from
  (`feat/<group-name>` by convention).
- **Merge strategy**: read from `config.toml`
  `[worktree.session_loop].merge_strategy`; default to `merge-commit` when
  unset.

## Step 1c. User approval

Wait for explicit user approval of the plan presented in Step 1b.

- **Approved** → continue to Step 1d.
- **Rejected or modified** → return to Step 1b, incorporate the user's
  feedback (different grouping, different WT count, different merge
  strategy), and re-present the plan. Do not proceed past this point without
  an explicit approval.

## Step 1d. WT creation/verification + branch setup

This step prepares the worktrees approved in Step 1c so that Steps 2-7 can run
inside each of them (spec: wiki/200-multi-wt-session-loop-integration.md §3.3).
Run 1d-1 through 1d-3 once, here, before handing off to the per-WT subagents.
Run 1d-4 later, per WT, once that WT's own Steps 2-7 have finished.

### 1d-1. Detect existing worktrees

```bash
git worktree list
```

Parse the output (`<path> <commit> [<branch>]`) to see which worktrees already
exist and which branch each one is on. A worktree already checked out on the
branch name from the Step 1b/1c plan is a reuse candidate; note its path for
1d-2.

### 1d-2. Create or reuse each group's worktree

For every group row in the approved plan, in order:

1. **Reuse check**: if `git worktree list` (1d-1) already shows a worktree on
   that group's branch, reuse it:
   ```bash
   git -C <existing-wt-path> checkout <branch-name>
   ```
   (a no-op if it's already checked out; makes sure a stale checkout doesn't
   silently run the wrong branch).
2. **Create if absent**:
   ```bash
   git worktree add ../<project>-wt<N> -b <branch-name>
   ```
   - `<project>` is the current repo's directory name.
   - `<N>` is the next free worktree number: take the highest `wtN` suffix
     seen in the `git worktree list` output (1d-1) and increment it (start at
     `wt2` — `wt1`/no-suffix is the primary worktree running this manager,
     which is never reassigned).
   - `<branch-name>` is exactly the branch from the approved plan (e.g.
     `feat/auth`).
3. **Concurrency limit**: before creating a *new* worktree, count how many
   worktrees this plan would have active at once (existing + newly created so
   far). If that count would exceed `[worktree.session_loop].max_concurrent_wts`
   from `config.toml` (default `4`, see `handoff_get_config`), stop and report
   an error to the user instead of creating it — do not silently cap the plan
   or drop a group. Let the user reduce the group count, raise the config
   limit, or split the work across sequential batches.

1d-2 produces a path + branch per group, ready for 1d-3.

### 1d-3. Prepare each WT's session

For each worktree from 1d-2 (this repeats once per WT — either done directly
if you enter the WT yourself, or as the first thing the per-WT subagent does
after `EnterWorktree`):

1. `handoff_load_context` — registers this agent against the shared `.handoff/`
   (worktrees share storage per Phase 1; this call is what makes the agent
   visible in `handoff_list_agents` / `handoff_overview`).
2. For every task assigned to this WT's group in the approved plan, call
   `handoff_claim_task(task_id, session_id)`.
   - **Claim failure** (task already held by another agent with a
     non-expired lease) — do not silently skip the task or reassign it
     yourself. Report it to the user: which task, which group, and that the
     WT assignment plan may need to be revisited (another session may already
     be working it, or a stale claim needs `handoff_reclaim_task`).
3. `handoff_save_context(session_status="active")` — opens this WT's own
   session. Its `scope` is auto-detected as `"worktree"` (Phase 2 / t250.1),
   and `parent_session_id` is auto-set to the primary worktree's active
   session — no extra argument needed for either.

Once 1d-3 completes for a WT, that WT is ready to run Steps 2-7 as its own
independent session-loop pass.

### 1d-4. Post-completion cleanup (extends Step 7)

This sub-step runs **per WT, after that WT's own Steps 2-7 have converged** —
i.e. after its session-loop pass reaches its own Step 7 (session close) and
before the manager's overall Step 8 (next session) for the *primary* session.
Do not run 1d-4 before a WT's tasks are actually done.

1. `handoff_release_task(task_id, revert_status="done")` for every task that
   WT claimed in 1d-3 — releases the claim while keeping the task in its
   current status. **Always pass `revert_status` explicitly** (the default
   is `"todo"`, which would silently revert a completed task back to the
   backlog).
2. `handoff_save_context(session_status="closed")` — closes that WT's own
   session, same as the primary session's Step 7.
3. **WT cleanup** — read `[worktree.session_loop].auto_cleanup` from
   `config.toml`:
   - `true`: remove the worktree automatically:
     ```bash
     git worktree remove ../<project>-wt<N>
     ```
     (the branch itself is left in place for the later merge step —
     only the worktree checkout is removed).
   - `false` (default): do **not** remove anything automatically. Ask the
     user for confirmation before running `git worktree remove`, since the
     worktree may still be needed for manual inspection or the pending merge.

## Step 5-wt. Per-WT Workflow dispatch

When Step 1b produced more than one group (approved in Step 1c), replace the
conventional Step 5 with this dispatch.

### 5-wt-1. Launch one subagent per WT group

For each row of the WT assignment plan approved in Step 1c, launch an `Agent`
tool call. **Launch all of them in the same message** — one Agent call per
group, issued together — so they run concurrently, not one after another.

Each subagent's prompt must give it everything it needs to run session-loop.md
Steps 2-7 by itself, inside its own WT, without re-reading this file:

- **Enter the worktree first**, before anything else:
  `EnterWorktree({ branch: "<group's branch>", path: "<group's WT path>" })`
  (both values come from the approved Step 1c plan / Step 1d WT setup).
- **Task scope**: the task IDs assigned to this group only (from the Step 1b
  plan), plus their `done_criteria` and any `instructions` already gathered
  in Step 2 for this group's tasks — do not make the subagent re-derive them.
- **Pipeline profile**: the profile chosen in Step 2b for this group's tasks
  (`express` / `standard` / `full`) — a subagent does not re-run 2b, it is
  told the answer.
- **Procedure summary**: an abridged version of Steps 2-7 (plan → mark
  in_progress → `Workflow(name: "handoff-task-loop:session-execute", args: {...})`
  → process results, check off done_criteria, file discovered issues →
  commit inside the WT branch → close its own session record) — a summary of
  the steps, not a verbatim copy of this document.
- **Report contract**: what to hand back to the manager on completion (see
  5-wt-3).

```javascript
// Illustrative shape — fill in per the approved Step 1c plan.
// Launch one such Agent call per WT group, all in the same message.
Agent({
  name: 'wt2-session',
  description: 'WT2 session for feature-auth',
  prompt: `
    You are a session manager running inside a worktree.

    1. Enter the worktree:
       EnterWorktree({ branch: "feat/auth", path: "../project-wt2" })

    2. Execute session-loop.md Steps 2-7 for tasks: t50, t51, t52
       - Use profile: standard
       - Use Workflow(name: "handoff-task-loop:session-execute", args: {...})
       - Follow the same procedure as the primary session-loop (task planning,
         in_progress marking, workflow launch, done_criteria check-off,
         discovered-issue filing, commit) — this WT's session-execute run is
         unmodified by multi-WT mode.

    3. After completion, commit changes on this WT's branch (per Step 6/7 of
       session-loop.md). Do not merge or push — merging across WTs is the
       manager's Step 9, run after every group reports back.

    4. Report back: session-execute result (passed/failed, dev_reports,
       pending_followups), the commit hash, and any discovered issues.
  `,
});
```

### 5-wt-2. Monitor progress while subagents run

While the subagents are executing, the manager may poll `handoff_overview`
periodically to check:

- Each WT's session state (`active` / `closed`).
- Each task's claim state and progress within its WT.
- Which agents are currently active in which WT.

This is observational only — the manager does not act on it mid-flight
except to answer the user if asked for status. It does not block or
serialize the subagents.

### 5-wt-3. Collect results

Wait for all subagents to complete (or fail) before moving to Step 6. From
each subagent's report, collect:

- The `session-execute` result for that WT (`passed`, `dev_reports`,
  `pending_followups`, discovered issues — same shape as the conventional
  Step 5 return value, since the subagent ran the identical Workflow).
- The commit hash(es) the subagent produced on its WT branch.
- Any issues the subagent discovered outside its assigned task scope.

### 5-wt-4. Fault isolation

One WT's failure must not block or corrupt the others:

- Each subagent runs independently — there is no shared mutable state between
  them beyond `.handoff/` itself, which already serializes concurrent writes.
- If one subagent crashes or its session fails, the others keep running to
  completion; do not cancel sibling subagents because one failed.
- A failed WT's tasks simply keep their `claim` — the lease TTL (Phase 1
  claim/release mechanism) releases it automatically, so a crashed subagent
  does not permanently lock those tasks out for a future session.
- The manager aggregates every WT's outcome (including failures) into a
  single report to the user in Step 6 — a failure in one group is reported
  alongside the successes of the others, not hidden by them.

## Step 6 multi-WT. Results processing

> The per-WT subagents already ran their own Step 6 (checked off `done_criteria`,
> marked their tasks `done`, filed discovered issues, committed) inside their own
> worktrees. The manager's job here is to aggregate, not repeat.

For each subagent's result collected in 5-wt-3:

1. **Check off done_criteria for real, don't just trust the subagent's claim.**
   The subagent already called `handoff_check_criterion` inside its own WT
   (since `.handoff/` is shared storage across worktrees per Phase 1), so
   this is a **verification pass**: confirm via `handoff_get_task` that the
   criteria the subagent reported as `met: true` are actually checked.
2. **File discovered issues** the subagent reported, exactly as the ordinary
   "On success" step 3 procedure — duplicate-check via `handoff_list_tasks`,
   then `handoff_update_task` (omit `id`) with the same title/labels/notes
   convention.
3. **Do not re-commit.** Each WT's subagent already committed on its own
   branch. Merging onto the target branch is Step 9.
4. **Aggregate a single report to the user**, per WT group: pass/fail,
   commit hash, discovered-issue count. Call out any failed or crashed group
   explicitly.
5. Proceed to Step 7 to close the **primary** session.

## Step 9. Merge orchestration

This step runs **only** after every WT group has reported back (5-wt-3) and
the multi-WT results processing (Step 6) is complete. When `auto_assign = false`
or grouping collapsed to a single group, skip Steps 9/10 entirely.

Do not run Step 9 while any WT subagent is still active, and do not merge a
group whose tasks did not converge without telling the user first.

### 9-1. Generate the merge plan

1. **Merge order** — from the dependency graph:
   - Groups with no dependency on another group merge first
   - Dependent groups merge after their dependencies
   - Break ties alphabetically (stable, deterministic)
2. **Merge strategy** — read `[worktree.session_loop].merge_strategy` from
   `config.toml`; default `merge-commit`. User may override per branch in 9-2.
3. **Conflict risk** — `git diff --name-only main...<branch>` per branch;
   compare file sets pairwise. Mark overlapping files with `warn:`.

**Present the plan to the user:**

```
### Merge plan

| Order | Branch     | Group         | Strategy     | Conflict risk                    |
|-------|------------|---------------|--------------|-----------------------------------|
| 1     | feat/auth  | feature-auth  | merge-commit | none                              |
| 2     | feat/api   | feature-api   | merge-commit | warn: src/main.rs (also in feat/auth) |

Change the strategy for any branch? (default: merge-commit)
Approve?
```

### 9-2. User approval

Wait for explicit approval. The user may change strategy per branch, reorder,
or drop branches. Rejected → do not merge, report in session close.

### 9-3. Sequential merge execution

Merge branches **one at a time, in approved order** — never in parallel.

```bash
# 1. Bring main up to date
git checkout main
git pull --ff-only origin main   # only if remote configured

# 2. Merge per approved strategy
# rebase-merge:
git checkout <branch> && git rebase main && git checkout main && git merge --ff-only <branch>
# merge-commit:
git merge --no-ff <branch>
# squash-merge:
git merge --squash <branch> && git commit -m "squash: <group-name> (<task-ids>)"
```

### 9-4. Post-merge gate check

After each successful merge, before moving to the next branch:

1. Run the project's quality gates (format, lint, type check, test)
2. **All green** → proceed to next branch
3. **Any fail** → stop merge sequence, report to user, ask for guidance

### 9-5. Conflict handling

If a merge produces a conflict:

1. **Abort immediately** — `git merge --abort` or `git rebase --abort`
2. **Notify the user** with specific files and branches
3. **User replies "continue"** → verify resolution, resume remaining merges
4. **User replies "cancel"** → stop, report merged vs. unmerged branches

### 9-6. Merge completion report

Report: which branches merged, in what order, with which strategy, resulting
`main` state.

## Step 10. WT cleanup

Runs after Step 9 completes or is abandoned. Only reachable in multi-WT mode.

1. **Delete merged branches**: `git branch -d <branch>` (not `-D` — only
   deletes fully merged branches)
2. **Remove worktrees** — read `auto_cleanup` from `config.toml`:
   - `true`: `git worktree remove ../<project>-wt<N>` automatically
   - `false` (default): ask user to confirm first
3. Agent records are GC'd automatically after 7 days — no action needed here.

## Multi-WT rules

- **Merges are sequential, never parallel** — a later branch depends on the previous
  branch already landing on `main`
- **Never merge past a conflict** — always abort and notify user
- **Do not push during merge orchestration** — local `main` only
- **Do not skip the post-merge gate check** — two individually-green branches
  are not guaranteed to stay green when merged
