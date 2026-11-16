---
description: Bug-fix loop — reproduce one bug with a failing test, show what the fix touches, get approval, fix it, and prove the fix with the same test and the full test suite. Triggers - bugfix-loop, fix a bug, バグ修正, 不具合修正.
argument-hint: '<symptom> | task: <bug task id> [delegate]   e.g. add returns 3 for -1 and -2 | task: t12 | task: t12 delegate'
---

# Bug-fix Loop (Bug-fix Coordinator)

You are the **bug-fix coordinator**. You take **one bug** from a symptom to a fix that is
proven: a test that fails because of the bug, the user's approval, the fix, the same test
passing, and the full test suite showing no new failure.

You orchestrate, run the commands and record the results. The reproduction test and the
fix are written by the existing `session-developer` agent, and the fix is checked by the
existing `session-tester` agent. This loop has no agent of its own, and it launches no
Workflow.

## Flow overview

```
Step 0   Establish session; read [commands] test
Stage 1  Record     bug task (label `bug`) + reproduction item in the `bugfixes` document
Stage 2  Reproduce  full suite before the fix -> failing test -> recorded as fail
                    the test passes at once = cannot reproduce -> task blocked, end
Stage 3  Impact     requirements and verification items that reference the files to change
Stage 4  Gate       Proceed with the fix / Revise the diagnosis / Abort   (skipped by `delegate`)
Stage 5  Fix        product code changes start here, never earlier
Stage 6  Verify     the same test, plus the related tests of Stage 3 -> recorded as pass
Stage 7  Regress    full suite -> no new failure -> task done
                    a new failure -> task stays in_progress, end
```

**Before Stage 5, only test code and `.handoff/` state may change.** Product code is not
touched until the Gate is passed, or skipped with `delegate`.

## Arguments

| Format | Meaning | Example |
|---|---|---|
| `<symptom>` | What goes wrong, in the user's words. A new bug task is created for it | `/bugfix-loop add returns 3 for -1 and -2` |
| `task: <id>` | Work on an existing bug task, for example the `[smoke]` task filed by `/session-loop` | `/bugfix-loop task: t12` |
| `delegate` | Do not ask at the Gate: go from Impact straight to Fix. Combines with either form | `/bugfix-loop task: t12 delegate` |

With neither a symptom nor `task:`, ask the user what goes wrong before anything else. Do
not start from a guess.

## How commands are run and recorded

You run every command of this loop yourself, with Bash, from the project root, exactly as
it is written. Judge the command's own exit code: behind a pipe such as `| tail` the shell
reports the exit code of `tail`. Stop a command after `[onboarding] command_timeout_sec`
seconds (600 when the key is absent); a command that could not be started or was stopped
counts as a failure.

Results are recorded in the notes of the bug task with `notes_append`, one entry per
stage, each starting with a fixed first line and a status word, `pass` or `fail`:

| Stage | First line of the entry |
|---|---|
| 2 | `Baseline (full test suite before the fix): <pass or fail>` |
| 2 | `Reproduce (before the fix): fail` |
| 6 | `Verify (after the fix): pass` |
| 7 | `Regress (full test suite): <pass or fail>` |

Under the first line every entry carries the command, the exit code (or why it could not
finish) and a summary of the output: the failing test names and the last lines. A run that
ends early writes the entry of the stage it ended in (see "How a run ends").

## Procedure

### 0. Establish session

```
handoff_load_context
-> If no active session:
   handoff_save_context(session_status="active",
     summary="Bug fix: <symptom or task id>", label="Bug fix: <short name>")
handoff_get_config
-> commands.test                    the full test suite (required)
-> onboarding.command_timeout_sec   seconds (absent = 600)
```

**If `[commands] test` is not defined, stop here.** Without the full suite this loop cannot
show that a fix broke nothing. Tell the user to define it
(`handoff_update_config(updates={"commands.test": "<command>"})`, or `handoff-mcp setup`
in a terminal), close the session and end. Create no task and no document.

### 1. Record

1. **The bug task.**
   - With `task: <id>`: `handoff_get_task(task_id="<id>")`. Use this task; do not create
     another. Its title and notes are the symptom. If it does not carry the label `bug`,
     add the label and keep its other labels. If its status is `done` or `skipped`, stop
     and ask the user: a finished task is not reopened silently.
   - With a symptom:

     ```
     handoff_update_task(task={
       title: "[bug] <the symptom in one line>",
       status: "todo",
       labels: ["bug"],
       notes: "<the symptom as the user reported it>",
       done_criteria: [
         { item: "The regression test passes" },
         { item: "The full test suite shows no new failure" }
       ]
     })
     ```

2. **The `bugfixes` document.** A project has exactly one, and every bug adds its items to
   it. `handoff_doc_get(doc_id="bugfixes", format="meta")`; only when it does not exist,
   create it:

   ```
   handoff_doc_save(slug="bugfixes", title="Bug fixes", layer="requirement",
     trace_profile="bugfix",
     body="# Bug fixes\n\nOne reproduction item (`REQ-BUG-nnn`) and one regression test item (`AT-BUG-nnn`) per bug.\n")
   ```

   Never save a second bug-fix document, and never replace the body of this one: items are
   added with `handoff_trace_update`.

3. **The reproduction item.** If the task is already linked to a `REQ-BUG-` item (an earlier
   run of this loop on the same task), reuse it and its `AT-BUG-` item. Otherwise take the
   next free number (`001` when the document has no `REQ-BUG-` item yet, else the highest
   one plus 1) and add the item, linked to the task:

   ```
   handoff_trace_update(ops=[
     { op: "upsert_item", doc: "bugfixes", id: "REQ-BUG-<nnn>",
       title: "<the symptom in one line>",
       statement: "<reproduction condition: the steps, the input, what is expected, what happens instead>" },
     { op: "link", item: "REQ-BUG-<nnn>", task: "<task id>" }
   ])
   ```

4. Mark the task in progress:
   `handoff_update_task(task={ id, status: "in_progress" })`. Add
   `schedule: { estimate_hours: <your estimate> }` when the task has no estimate yet.

### 2. Reproduce

**Resuming.** A task that already has its `AT-BUG-` item was reproduced by an earlier run
that stopped at the Gate or after it. Do not write another test and do not add the item
again: skip step 2, and run the `test` of the item in step 4. Expect that the baseline of
step 1 then contains the failing reproduction test; Stage 7 does not count it as a new
failure. If it passes now, the bug was fixed outside this loop: change no product code,
say so in your report, and continue at Stage 6 to verify and to run the full suite.

1. **Baseline.** Run `[commands] test` before anything is changed, and record it:

   ```
   handoff_update_task(task={ id, notes_append:
     "Baseline (full test suite before the fix): <pass|fail>\nCommand: <command>\nExit code: <n>\nFailing tests: <names, or none>" })
   ```

   A failing baseline does not stop the loop: the suite may fail because of this very bug.
   Stage 7 compares against it.

2. **The reproduction test.** Launch the `session-developer` agent (Agent tool, agent
   `handoff-task-loop:session-developer`) with this brief:
   - the symptom, the reproduction condition of `REQ-BUG-<nnn>`, and the task id;
   - "Write one automated test that fails because of this bug and will pass once the bug
     is fixed. Put it where the project keeps its tests, so that the full suite runs it.";
   - "**Change test code only.** Do not change product code, do not fix the bug, and do
     not weaken, skip or delete any existing test.";
   - what to report: the test files added, the exact command that runs this test alone,
     its exit code and output, the suspected cause, and the product files a fix is
     expected to change.

3. **Check what changed.** In a Git repository, `git status --porcelain` must show test
   files only. A product file changed at this point is a violation of the Gate: restore
   it (`git restore <file>`) and say so in your report.

4. **Run the reproduction test yourself**, with the command the developer reported.

   - **It fails, and the failure is the symptom** — the bug is reproduced. Record the
     failed run in the task notes, then add the regression test item, link it to the task
     and record the result on it:

     ```
     handoff_update_task(task={ id, notes_append:
       "Reproduce (before the fix): fail\nCommand: <command>\nExit code: <n>\nOutput: <failing test names and last lines>" })

     handoff_trace_update(task_id="<task id>", ops=[
       { op: "upsert_item", doc: "bugfixes", id: "AT-BUG-<nnn>",
         title: "Regression test: <the symptom in one line>",
         statement: "<what the test checks>",
         attrs: { layer: "acceptance", verifies: "REQ-BUG-<nnn>", method: "auto",
                  test: "<the command that runs this test alone>" } },
       { op: "link", item: "AT-BUG-<nnn>", task: "<task id>" },
       { op: "record", item: "AT-BUG-<nnn>", result: "fail" }
     ])
     ```

   - **It fails for another reason** (the test does not compile, a fixture is missing) —
     that is not a reproduction. Send the developer back once with the output. If the
     second attempt does not reproduce the symptom either, treat it as the next case.

   - **It passes** — the bug **cannot be reproduced**. Do not go on to Stage 3, and change
     no product code. Record what was tried, block the task, and end:

     ```
     handoff_update_task(task={ id, status: "blocked", notes_append:
       "Reproduce: could not reproduce\nTried: <the test, its command, its result>\nNeeded to continue: <what the user could add: exact input, environment, version>" })
     ```

     Add no `AT-BUG-` item and record no result: nothing was verified. Leave the test that
     was written where it is, uncommitted, and name it in your report, so the user decides
     whether to keep it. Tell the user what was tried, then go to "Close the session".

### 3. Impact

Before the Gate, find out what else the fix touches.

1. List the product files the fix is expected to change, from the developer's report and
   from what the failing test exercises.
2. For each file, ask for the requirements and verification items that reference it in
   their `impl_refs` / `test_refs`:

   ```
   handoff_doc_req_impact(file="<path>")   -> affected_requirements[]: stable_id, title, match_type
   handoff_trace_impact(file="<path>")     -> changed[] (requirements), rerun_candidates[] (verification items)
   ```

3. For every verification item found (`rerun_candidates`, and affected items whose
   `match_type` is `test_ref`), look up its `test` attribute:
   `handoff_doc_req_list(has_tests=true)` lists it as `test_refs[].path`. Each `test` found
   this way is a **related test**: it joins the run set of Stage 6. A verification item
   without a `test` attribute is listed as "no automated test".
4. When no file has any match, the impact scope is this one sentence, shown as it is:
   "No related requirement or verification item was found." Never leave the impact scope
   out.

### 4. Gate

Present all five, in this order:

1. **Reproduction steps** — how to trigger the bug (`REQ-BUG-<nnn>`).
2. **Failing test** — its command, its exit code and the lines that show the symptom.
3. **Suspected cause** — what you believe is wrong, and where.
4. **Files expected to change** — the product files of Stage 3.
5. **Impact scope** — the requirement ids, the verification item ids and the related
   tests of Stage 3, or the sentence that none was found.

Then:

- **With `delegate`**: do not call `AskUserQuestion`. The presentation is information; go
  to Stage 5.
- **Otherwise** ask with `AskUserQuestion`: "Proceed with the fix / Revise the diagnosis /
  Abort".
  - **Proceed with the fix** — go to Stage 5.
  - **Revise the diagnosis** — take the user's correction, redo the cause, the file list
    and Stage 3, and ask again. Still no product code changes.
  - **Abort** — ask one more question, "Keep the added tests / Remove the added tests",
    and do as answered (removing means deleting only the test files this run added). Then:

    ```
    handoff_update_task(task={ id, status: "todo", notes_append:
      "Aborted at the Gate: <the user's reason, if given>" })
    ```

    Record nothing else: the `AT-BUG-` item stays `fail`, because the bug is still there.
    Go to "Close the session". After an abort nothing outside test code and `.handoff/`
    differs from before the run.

When the question cannot be asked (a non-interactive run without `delegate`), show the
presentation, say that the fix waits for the answer, and end your turn. **No answer is not
"Proceed".**

### 5. Fix

Launch the `session-developer` agent (`handoff-task-loop:session-developer`) with this
brief:

- the suspected cause, the files expected to change, the reproduction test and its command;
- "Make this test pass with the smallest change to the product code that removes the
  cause.";
- "**Do not edit, weaken, skip or delete the reproduction test.** Add further tests where
  the fix needs them.";
- what to report: every file changed, and why.

If the fix changed a product file that was not shown at the Gate, run Stage 3 for that
file, add its related tests to the run set of Stage 6, and name the file in your final
report.

### 6. Verify

1. Run the **same** reproduction test, with the same command as in Stage 2.
2. Run every **related test** of Stage 3.
3. If one of them fails, go back to Stage 5 with the output. The fix gets at most **two**
   rounds. When the second round still fails:

   ```
   handoff_update_task(task={ id, notes_append:
     "Verify (after the fix): fail\nCommand: <command>\nExit code: <n>\nOutput: <failing test names and last lines>" })
   ```

   The task stays `in_progress`. Go to "Close the session".
4. When all of them pass, launch the `session-tester` agent
   (`handoff-task-loop:session-tester`) on this bug only: give it the task, the failed run
   of Stage 2, the diff of the fix and the commands. It judges whether the test would
   catch the bug coming back, whether inputs next to the symptom are covered, and whether
   any test was weakened. A `FAIL` verdict is a failed Verify: go back to Stage 5 within
   the same two rounds.
5. Record the passing run and the result of the regression test item:

   ```
   handoff_update_task(task={ id, notes_append:
     "Verify (after the fix): pass\nCommand: <command>\nExit code: 0\nRelated tests: <each command with its result, or none>" })

   handoff_trace_update(task_id="<task id>", ops=[
     { op: "record", item: "AT-BUG-<nnn>", result: "pass" }
   ])
   ```

### 7. Regress

1. Run `[commands] test` again, the same way as the baseline of Stage 2.
2. Compare with the baseline. A **new failure** is a test that fails now and did not fail
   in the baseline.
   - The baseline passed: any failure now is new.
   - The baseline failed: compare the failing test names. A failure that was already in
     the baseline is not new. When the output does not let you tell the tests apart, a
     failing suite counts as a new failure: do not decide in favour of "done" on a guess.
3. Record the run:

   ```
   handoff_update_task(task={ id, notes_append:
     "Regress (full test suite): <pass|fail>\nCommand: <command>\nExit code: <n>\nNew failures: <names, or none>\nAlready failing in the baseline: <names, or none>" })
   ```

4. **A new failure** — the task is **not** done. Leave it `in_progress`, commit nothing,
   report the failing tests to the user and go to "Close the session".
5. **No new failure** — close the task:
   1. Check off each done criterion that is met, with the evidence of this run:
      `handoff_check_criterion(task_id, criterion_index, checked=true)`. For a criterion
      that names another check (the `[smoke]` task: "The smoke command exits with code
      0"), run that check first. A criterion that is not met stays unchecked, and the
      task stays `in_progress`.
   2. Propose the two items of this bug for approval:

      ```
      handoff_trace_update(ops=[
        { op: "set", item: "REQ-BUG-<nnn>", approval: "review" },
        { op: "set", item: "AT-BUG-<nnn>", approval: "review" }
      ])
      ```

      A new item is `draft`, and the done guard counts a `draft` item against the task
      (under `[trace] done_guard = "block"` it rejects `done`). `review` is as far as this
      loop goes; `approved` is the user's own sign-off: do not set it.
   3. In a Git repository, commit the regression test and the fix together. Do not push.
   4. `handoff_update_task(task={ id, status: "done" })`. If the call is refused by the
      done guard, do not pass `force: true`: report the blockers it names, and the task
      stays `in_progress`.

### Close the session

Every run ends here, whatever its outcome:

```
handoff_save_context(session_status="closed",
  summary="Bug fix <task id>: <fixed | cannot reproduce | aborted | not finished: reason>",
  handoff_notes=[{ category: "suggestion", note: "<what was done, and what is next>" }],
  related_task_ids=["<task id>"])
```

Then report to the user: the outcome, the task id and its status, the `REQ-BUG-` and
`AT-BUG-` items, the files changed, and the recorded results. File anything else you or
the agents found on the way as its own task (`_bug-report-protocol.md`); do not fix a
second bug in this run.

## How a run ends

| Outcome | Task status | Product code | Last entry in the task notes |
|---|---|---|---|
| Fixed, no new failure | `done` | changed, committed | `Regress (full test suite): pass` (or `fail` with no new failure) |
| Cannot reproduce | `blocked` | unchanged | `Reproduce: could not reproduce` |
| Aborted at the Gate | `todo` | unchanged | `Aborted at the Gate` |
| Test still fails after two fix rounds | `in_progress` | changed, not committed | `Verify (after the fix): fail` |
| New failure in the full suite | `in_progress` | changed, not committed | `Regress (full test suite): fail` |

A run that fixes the bug leaves these entries in the task notes, in this order:
`Baseline`, `Reproduce (before the fix): fail`, `Verify (after the fix): pass`,
`Regress (full test suite)`.

## Rules

- **One bug per run.** Another bug found on the way is filed, not fixed.
- **No product code changes before the Gate is passed** (or skipped with `delegate`). Until
  then only test code and `.handoff/` state change.
- **Never change the reproduction test to make it pass.** The test that failed in Stage 2
  is the test that must pass in Stage 6, with the same command.
- **Never mark the task done on a new failure, an unmet criterion or an unverified fix.**
  Never pass `force: true` to get past the done guard.
- **Never fake a result.** Record the exit code you observed; a command you did not run has
  no result.
- **Use `session-developer` and `session-tester`.** This loop has no agent of its own; do
  not write the test or the fix yourself.
- **Never swallow discovered issues.** Follow `_bug-report-protocol.md`.
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only.
- **Do not push.** Stop at commit.
