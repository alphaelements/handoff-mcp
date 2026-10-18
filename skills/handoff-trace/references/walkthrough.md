# Walkthrough — one requirement through the whole V

A complete, runnable example under the `standard` profile: one requirement,
one basic spec, one system test, generated acceptance tests, an implementing
task, automated and manual results, a verification campaign, a requirement
change with suspect review, approval, a snapshot and a report.

Each step shows the exact call as JSON: `tool` and `args`. `expect` lists
strings the response contains, and `save` names a value from the response
that later steps reuse as `${name}`. `project_dir` is omitted (the server's
working directory is the project). This file is executed as-is by the
project's test suite, so every call here is known to work.

## 1. Set up

Initialize the project and choose the `standard` profile (requirement, basic
spec, acceptance, system test).

```json walkthrough
{"tool": "handoff_init", "args": {"project_name": "walkthrough"}}
```

```json walkthrough
{"tool": "handoff_update_config",
 "args": {"updates": {"trace.profile": "standard", "trace.done_guard": "warn"}},
 "expect": ["trace.profile = \"standard\""]}
```

## 2. Write the requirement, the spec and the system test

The requirement carries acceptance criteria. Attributes come right after the
heading.

```json walkthrough
{"tool": "handoff_doc_save",
 "args": {"slug": "auth-requirements", "title": "Auth Requirements", "layer": "requirement",
          "body": "# Auth Requirements\n\n### FR-001 Lock the account after repeated login failures\n\n- priority: P1\n\nLock an account for 15 minutes after 5 consecutive failed logins.\n\n受入基準:\n- AC1: Given 4 failed attempts When the 5th attempt fails Then the account is locked\n- AC2: Given a locked account When 15 minutes pass Then the user can log in again\n"},
 "expect": ["\"slug\": \"auth-requirements\""]}
```

The basic spec `refines` the requirement:

```json walkthrough
{"tool": "handoff_doc_save",
 "args": {"slug": "auth-spec", "title": "Auth Basic Spec", "layer": "basic_spec",
          "body": "# Auth Basic Spec\n\n### SPEC-001 Failed-login counter\n\n- refines: FR-001\n\nThe auth service keeps a per-account counter of consecutive failures, reset on success, and stores `locked_until`.\n"},
 "expect": ["\"slug\": \"auth-spec\""]}
```

The system test `verifies` the spec. Its `test:` value is the test's name as
the runner reports it (here a test `locks_after_five_failures` in the `auth`
test binary), not a file path:

```json walkthrough
{"tool": "handoff_doc_save",
 "args": {"slug": "auth-system-tests", "title": "Auth System Tests", "layer": "system_test",
          "body": "# Auth System Tests\n\n### ST-001 Counter locks on the fifth failure\n\n- verifies: SPEC-001\n- method: auto\n- test: auth::locks_after_five_failures\n\nFive failed logins set `locked_until` 15 minutes ahead.\n"},
 "expect": ["\"slug\": \"auth-system-tests\""]}
```

## 3. Generate acceptance tests from the criteria

Under `standard`, acceptance criteria are not verification items by
themselves. Create an (empty) acceptance document and let `trace_scaffold`
write one item per criterion:

```json walkthrough
{"tool": "handoff_doc_save",
 "args": {"slug": "auth-acceptance", "title": "Auth Acceptance Tests", "layer": "acceptance",
          "body": "# Auth Acceptance Tests\n"}}
```

```json walkthrough
{"tool": "handoff_trace_scaffold",
 "args": {"doc": "auth-requirements", "target_doc": "auth-acceptance", "mode": "apply"},
 "expect": ["AT-FR-001-1", "AT-FR-001-2", "\"applied\": true"]}
```

Run with `mode: "preview"` first when you want to review. The items get
`verifies: FR-001`, `from: FR-001#AC1` and `method: manual`; re-running
skips criteria that already have an item.

Check the whole graph:

```json walkthrough
{"tool": "handoff_trace_report", "args": {},
 "expect": ["\"source\": \"profile\"", "\"system_test\""]}
```

Everything is `not_run` so far, and `SPEC-001` has no implementing task.

## 4. Link the implementing task

```json walkthrough
{"tool": "handoff_update_task",
 "args": {"task": {"id": "t1", "title": "Implement account lockout", "status": "in_progress",
                   "requirement_ids": ["SPEC-001"], "schedule": {"estimate_hours": 4}}},
 "expect": ["t1"]}
```

The task's working context — what it implements, the upstream requirement,
the verifying test:

```json walkthrough
{"tool": "handoff_trace_slice", "args": {"task_id": "t1"},
 "expect": ["SPEC-001", "FR-001", "ST-001", "\"role\": \"implements\""]}
```

## 5. Record the automated test result

Feed the runner's JUnit XML (for example from `cargo nextest run` with a
JUnit profile — pass the file with `output_file`, relative to the project directory or absolute). JUnit names are
`classname::name`, so `walk::auth::locks_after_five_failures` matches
`test: auth::locks_after_five_failures` as a `::`-suffix:

```json walkthrough
{"tool": "handoff_trace_ingest",
 "args": {"format": "junit_xml", "task_id": "t1",
          "output": "<testsuites><testsuite name=\"walk::auth\" tests=\"1\"><testcase classname=\"walk::auth\" name=\"locks_after_five_failures\"/></testsuite></testsuites>"},
 "expect": ["\"recorded\": 1", "\"item\": \"ST-001\""]}
```

## 6. Run the manual acceptance tests as a campaign

A campaign fixes the target set, keeps a checklist with evidence, and is
approved at the end:

```json walkthrough
{"tool": "handoff_trace_test_run",
 "args": {"action": "create", "label": "Release 1.0 acceptance", "scope": {"layers": ["acceptance"]}},
 "expect": ["\"checklist_count\": 2"],
 "save": {"run": "/test_run_id"}}
```

Record the results in the trace graph **with the campaign id** — this is what
changes the items' `state`:

```json walkthrough
{"tool": "handoff_trace_record",
 "args": {"test_run_id": "${run}", "executor_kind": "human", "executor_id": "alice",
          "results": [{"item": "AT-FR-001-1", "result": "pass", "evidence": ["evidence/lock.png"]},
                      {"item": "AT-FR-001-2", "result": "pass"}]},
 "expect": ["\"recorded\": 2"]}
```

Tick the campaign checklist (this updates only the campaign, not `state`):

```json walkthrough
{"tool": "handoff_trace_test_run",
 "args": {"action": "record_check", "test_run_id": "${run}", "item_id": "AT-FR-001-1", "result": "pass",
          "verified_by": "alice",
          "evidence": [{"path": "evidence/lock.png", "type": "screenshot", "caption": "locked after 5 tries"}]},
 "expect": ["\"campaign_status\": \"in_progress\""]}
```

```json walkthrough
{"tool": "handoff_trace_test_run",
 "args": {"action": "record_check", "test_run_id": "${run}", "item_id": "AT-FR-001-2", "result": "pass",
          "verified_by": "alice"},
 "expect": ["\"pending\": 0"]}
```

Close and approve the campaign (approval is a person's decision):

```json walkthrough
{"tool": "handoff_trace_test_run",
 "args": {"action": "set_status", "test_run_id": "${run}", "status": "completed"},
 "expect": ["\"campaign_status\": \"completed\""]}
```

```json walkthrough
{"tool": "handoff_trace_test_run",
 "args": {"action": "set_status", "test_run_id": "${run}", "status": "approved", "approved_by": "bob"},
 "expect": ["\"campaign_status\": \"approved\""]}
```

## 7. Change the requirement

The requirement is clarified: failures are counted per account across all
devices. First see what the edit affects — pass the whole new item block,
heading and acceptance criteria included:

```json walkthrough
{"tool": "handoff_trace_impact",
 "args": {"item": "FR-001",
          "proposed": "### FR-001 Lock the account after repeated login failures\n\n- priority: P1\n\nLock an account for 15 minutes after 5 consecutive failed logins, counted across all devices.\n\n受入基準:\n- AC1: Given 4 failed attempts When the 5th attempt fails Then the account is locked\n- AC2: Given a locked account When 15 minutes pass Then the user can log in again\n"},
 "expect": ["would_suspect", "\"rerun_candidates\"", "AT-FR-001-1", "SPEC-001"]}
```

Apply the edit. The response reports what turned suspect:

```json walkthrough
{"tool": "handoff_trace_update",
 "args": {"ops": [{"op": "upsert_item", "doc": "auth-requirements", "id": "FR-001",
                   "statement": "Lock an account for 15 minutes after 5 consecutive failed logins, counted across all devices."}]},
 "expect": ["suspect_introduced", "\"reverify\""]}
```

```json walkthrough
{"tool": "handoff_trace_suspect", "args": {"action": "list"},
 "expect": ["\"link\": 3", "\"upstream\": \"FR-001\""]}
```

Review each one: read `SPEC-001` and the acceptance items against the new
text. The per-account counter already covers every device and the
acceptance steps still hold, so clear the links pointing at `FR-001` with a
reason (an audit file is written). Had the criteria or the spec become wrong,
you would fix those items first and clear afterwards:

```json walkthrough
{"tool": "handoff_trace_suspect",
 "args": {"action": "clear", "targets": [{"upstream": "FR-001"}],
          "reason": "Clarification only: the per-account counter already spans devices; acceptance steps unchanged"},
 "expect": ["clear_id"]}
```

The acceptance tests were flagged `reverify`: their passing results predate
the change. Re-run and record them:

```json walkthrough
{"tool": "handoff_trace_update",
 "args": {"ops": [{"op": "record", "item": "AT-FR-001-1", "result": "pass", "note": "re-run after the clarification"},
                  {"op": "record", "item": "AT-FR-001-2", "result": "pass"}],
          "executor_kind": "human", "executor_id": "alice"},
 "expect": ["\"op\": \"record\""]}
```

## 8. Approve and finish

Approve the definition items (a person's action; the definition hash at
approval is stored, and a later edit drops the item back to `draft`):

```json walkthrough
{"tool": "handoff_trace_update",
 "args": {"ops": [{"op": "set", "item": "FR-001", "approval": "approved"},
                  {"op": "set", "item": "SPEC-001", "approval": "approved"}],
          "executor_kind": "human", "executor_id": "bob"},
 "expect": ["\"approval\": \"approved\""]}
```

Close the task. The done guard checks the linked items' blockers (failing or
unrun verifiers, suspects, draft approval) first:

```json walkthrough
{"tool": "handoff_update_task", "args": {"task": {"id": "t1", "status": "done"}},
 "expect": ["[done]"]}
```

Every layer is now `verified`:

```json walkthrough
{"tool": "handoff_trace_report", "args": {},
 "expect": ["\"project_status\": \"verified\"", "\"gaps\": []"]}
```

Mark a layer reviewed and approved (allowed only while it is `verified`):

```json walkthrough
{"tool": "handoff_trace_update",
 "args": {"ops": [{"op": "set_layer_status", "layer": "requirement", "status": "approved"}]},
 "expect": ["\"op\": \"set_layer_status\""]}
```

## 9. Snapshot and report

Record a project baseline (coverage and state at release; `diff` against it
later shows regressions):

```json walkthrough
{"tool": "handoff_trace_baseline", "args": {"action": "create", "label": "release-1.0"},
 "expect": ["baseline_id", "\"items_count\": 5"]}
```

Generate the verification report, submit it, and approve it:

```json walkthrough
{"tool": "handoff_report",
 "args": {"action": "generate", "report_type": "verification", "scope": {"layers": ["acceptance", "system_test"]}},
 "expect": ["\"status\": \"draft\""],
 "save": {"report": "/report/report_id"}}
```

```json walkthrough
{"tool": "handoff_report", "args": {"action": "submit", "report_id": "${report}"},
 "expect": ["\"status\": \"submitted\""]}
```

```json walkthrough
{"tool": "handoff_report", "args": {"action": "approve", "report_id": "${report}", "reviewer": "bob"},
 "expect": ["\"status\": \"approved\""]}
```

The report body is under `.handoff/reports/`. With the approved campaign, an
`inspection` report (`scope: {"campaign": "<test_run_id>"}`) produces the
inspection certificate; once every layer is approved, a `completion` report
becomes available.

Finally, lint has no errors:

```json walkthrough
{"tool": "handoff_trace_lint", "args": {},
 "expect": ["\"exit_code\": 0", "\"error\": 0"]}
```

## 10. Attributes survive a rewrite

`upsert_item` merges `attrs` key by key. Set an assignee and a `needs` list
once, then rewrite the item with another attribute: both are kept (attribute
changes do not touch the definition hash, so nothing turns suspect).

```json walkthrough
{"tool": "handoff_trace_update",
 "args": {"ops": [{"op": "upsert_item", "doc": "auth-requirements", "id": "FR-001",
                   "attrs": {"assignee": "alice", "needs": "system_test"}}]},
 "expect": ["\"op\": \"upsert_item\""]}
```

```json walkthrough
{"tool": "handoff_trace_update",
 "args": {"ops": [{"op": "upsert_item", "doc": "auth-requirements", "id": "FR-001",
                   "attrs": {"priority": "P0"}}]},
 "expect": ["\"op\": \"upsert_item\""]}
```

```json walkthrough
{"tool": "handoff_doc_get", "args": {"doc_id": "auth-requirements"},
 "expect": ["- assignee: alice", "- needs: system_test", "- priority: P0"]}
```

## 11. Propose a change for review

When an agent should only *suggest* a change and a person decides, run the
same ops with `propose: true`. Nothing is written until the delta is applied.
A call-level `task_id` is copied into each `link` op, so the stored proposal
needs no other context:

```json walkthrough
{"tool": "handoff_update_task",
 "args": {"task": {"id": "t2", "title": "Harden lockout", "status": "todo"}},
 "expect": ["t2"]}
```

```json walkthrough
{"tool": "handoff_trace_update",
 "args": {"propose": true, "task_id": "t2", "description": "Link t2 to the lockout requirement",
          "ops": [{"op": "link", "item": "FR-001"}]},
 "expect": ["\"propose\": true", "\"warnings\""],
 "save": {"delta": "/delta_id"}}
```

```json walkthrough
{"tool": "handoff_trace_delta", "args": {"action": "list"},
 "expect": ["Link t2 to the lockout requirement", "\"stale\": false"]}
```

A person reviews the previews and applies it:

```json walkthrough
{"tool": "handoff_trace_delta",
 "args": {"action": "apply", "delta_id": "${delta}", "executor_kind": "human", "executor_id": "bob"},
 "expect": ["\"op\": \"link\"", "\"remainder_delta_id\": null"]}
```

```json walkthrough
{"tool": "handoff_trace_slice", "args": {"task_id": "t2"},
 "expect": ["FR-001"]}
```

## What this example skipped

- Requirement trees (`parent`, `kind`, `condition`) — see the
  `handoff-requirements` skill's requirement-tree reference.
- Bugs: on a `fail` result, open a task labelled `bug` (plus a
  `bug:<fix|defer|waive|not_a_bug>` disposition) linked to the failing item;
  the `defect` report lists them — see `verification.md`.
- Partial apply of a proposal (`op_indices`), rejecting one, and the
  staleness check — see `change-management.md`.
