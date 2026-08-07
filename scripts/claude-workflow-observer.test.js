import assert from "node:assert/strict";
import test from "node:test";
import { commandName, normalize, summarize } from "./claude-workflow-observer.js";

test("normalizes lifecycle events without retaining prompt or command body", () => {
  const event = normalize({ hook_event_name: "PreToolUse", session_id: "s1", tool_use_id: "u1", tool_name: "Bash", tool_input: { command: "TOKEN=secret cargo test --all" }, prompt: "sensitive" }, new Date("2026-08-07T00:00:00Z"));
  assert.deepEqual(event, { schema_version: 1, event: "tool_started", observed_at: "2026-08-07T00:00:00.000Z", hook_event: "PreToolUse", agent_id: "s1", run_id: "s1", session_id: "s1", tool_use_id: "u1", tool_name: "Bash", command: "cargo" });
  assert.equal(JSON.stringify(event).includes("secret"), false);
  assert.equal(JSON.stringify(event).includes("sensitive"), false);
  assert.equal(commandName("Bash", { command: "echo hello | cargo test" }), "echo");
});

test("preserves wrapper-supplied workflow correlation without requiring it from Claude", () => {
  const saved = { ...process.env };
  process.env.HANDOFF_WORKFLOW_RUN_ID = "run-parent";
  process.env.HANDOFF_WORKFLOW_TASK_ID = "t226.1";
  process.env.HANDOFF_WORKFLOW_PHASE = "implement";
  try {
    const event = normalize({ hook_event_name: "SubagentStart", session_id: "child-session", agent_id: "child-agent", parent_session_id: "parent-session" }, new Date("2026-08-07T00:00:00Z"));
    assert.equal(event.run_id, "run-parent");
    assert.equal(event.agent_id, "child-agent");
    assert.equal(event.parent_run_id, "parent-session");
    assert.equal(event.task_id, "t226.1");
    assert.equal(event.phase, "implement");
  } finally {
    for (const key of ["HANDOFF_WORKFLOW_RUN_ID", "HANDOFF_WORKFLOW_TASK_ID", "HANDOFF_WORKFLOW_PHASE"]) {
      if (saved[key] === undefined) delete process.env[key]; else process.env[key] = saved[key];
    }
  }
});

test("summary remains running until an explicit agent finish event", () => {
  const events = [
    normalize({ hook_event_name: "SessionStart", session_id: "s1" }, new Date("2026-08-07T00:00:00Z")),
    normalize({ hook_event_name: "UserPromptSubmit", session_id: "s1" }, new Date("2026-08-07T00:00:01Z")),
    normalize({ hook_event_name: "PreToolUse", session_id: "s1", tool_use_id: "u1", tool_name: "Bash", tool_input: { command: "cargo test" } }, new Date("2026-08-07T00:00:02Z")),
    normalize({ hook_event_name: "PostToolUse", session_id: "s1", tool_use_id: "u1", tool_name: "Bash", tool_input: { command: "cargo test" } }, new Date("2026-08-07T00:00:05Z")),
  ];
  assert.deepEqual(summarize(events), [{ run_id: "s1", status: "running", turn_count: 1, tool_count: 1, tool_wait_ms: 3000, model_wait_ms_estimate: 2000, command_durations_ms: { cargo: 3000 }, longest_command: { command: "cargo", duration_ms: 3000 } }]);
  events.push(normalize({ hook_event_name: "SessionEnd", session_id: "s1" }, new Date("2026-08-07T00:00:07Z")));
  assert.equal(summarize(events)[0].status, "finished");
});

test("shared run stays running while any agent is still active", () => {
  const RUN = "workflow-run-1";
  const saved = { ...process.env };
  process.env.HANDOFF_WORKFLOW_RUN_ID = RUN;
  try {
    const events = [
      normalize({ hook_event_name: "SessionStart", session_id: "root", agent_id: "root" }, new Date("2026-08-07T00:00:00Z")),
      normalize({ hook_event_name: "SubagentStart", session_id: "child-a", agent_id: "child-a", parent_session_id: "root" }, new Date("2026-08-07T00:00:01Z")),
      normalize({ hook_event_name: "SubagentStart", session_id: "child-b", agent_id: "child-b", parent_session_id: "root" }, new Date("2026-08-07T00:00:02Z")),
      // child-a finishes but child-b and root are still active
      normalize({ hook_event_name: "SubagentStop", session_id: "child-a", agent_id: "child-a", parent_session_id: "root" }, new Date("2026-08-07T00:00:10Z")),
    ];
    const mid = summarize(events);
    assert.equal(mid.length, 1, "all events share one run_id");
    assert.equal(mid[0].run_id, RUN);
    assert.equal(mid[0].status, "running", "must stay running while child-b and root are active");

    // child-b finishes — root still active
    events.push(normalize({ hook_event_name: "SubagentStop", session_id: "child-b", agent_id: "child-b", parent_session_id: "root" }, new Date("2026-08-07T00:00:20Z")));
    assert.equal(summarize(events)[0].status, "running", "must stay running while root is active");

    // root finishes — now all agents are done
    events.push(normalize({ hook_event_name: "SessionEnd", session_id: "root", agent_id: "root" }, new Date("2026-08-07T00:00:30Z")));
    assert.equal(summarize(events)[0].status, "finished", "finished only when all agents are done");
  } finally {
    for (const key of ["HANDOFF_WORKFLOW_RUN_ID"]) {
      if (saved[key] === undefined) delete process.env[key]; else process.env[key] = saved[key];
    }
  }
});

test("asymmetric agent_id between start and stop does not leave a run stuck as running", () => {
  const RUN = "workflow-run-2";
  const saved = { ...process.env };
  process.env.HANDOFF_WORKFLOW_RUN_ID = RUN;
  try {
    const events = [
      // root starts with explicit agent_id
      normalize({ hook_event_name: "SessionStart", session_id: "sess-root", agent_id: "agent-root" }, new Date("2026-08-07T00:00:00Z")),
      // child starts with agent_id, stops WITHOUT agent_id (falls back to session_id)
      normalize({ hook_event_name: "SubagentStart", session_id: "sess-child", agent_id: "agent-child", parent_session_id: "sess-root" }, new Date("2026-08-07T00:00:01Z")),
      normalize({ hook_event_name: "SubagentStop", session_id: "sess-child", parent_session_id: "sess-root" }, new Date("2026-08-07T00:00:10Z")),
    ];
    // child finished (via session_id fallback), root still active
    assert.equal(summarize(events)[0].status, "running", "root is still active");

    // root stops without agent_id (falls back to session_id)
    events.push(normalize({ hook_event_name: "SessionEnd", session_id: "sess-root" }, new Date("2026-08-07T00:00:20Z")));
    assert.equal(summarize(events)[0].status, "finished", "must not stay stuck as running when all agents finished via session_id fallback");
  } finally {
    for (const key of ["HANDOFF_WORKFLOW_RUN_ID"]) {
      if (saved[key] === undefined) delete process.env[key]; else process.env[key] = saved[key];
    }
  }
});

test("reverse asymmetry: start without agent_id, stop with agent_id", () => {
  const RUN = "workflow-run-3";
  const saved = { ...process.env };
  process.env.HANDOFF_WORKFLOW_RUN_ID = RUN;
  try {
    const events = [
      // starts without agent_id (falls back to session_id)
      normalize({ hook_event_name: "SubagentStart", session_id: "sess-x", parent_session_id: "root" }, new Date("2026-08-07T00:00:00Z")),
      // stops WITH agent_id
      normalize({ hook_event_name: "SubagentStop", session_id: "sess-x", agent_id: "agent-x", parent_session_id: "root" }, new Date("2026-08-07T00:00:10Z")),
    ];
    assert.equal(summarize(events)[0].status, "finished", "must not stay stuck when stop has agent_id that start lacked");
  } finally {
    for (const key of ["HANDOFF_WORKFLOW_RUN_ID"]) {
      if (saved[key] === undefined) delete process.env[key]; else process.env[key] = saved[key];
    }
  }
});
