import assert from "node:assert/strict";
import test from "node:test";
import { JobController } from "./claude-workflow-job-controller.js";

// --- helpers ---

function makeEvent(overrides) {
  return {
    schema_version: 1,
    event: "tool_started",
    observed_at: new Date().toISOString(),
    hook_event: "PreToolUse",
    agent_id: "agent-1",
    run_id: "run-1",
    session_id: "sess-1",
    tool_use_id: "u1",
    tool_name: "Bash",
    command: "cargo",
    ...overrides,
  };
}

function toolStarted(overrides) {
  return makeEvent({ event: "tool_started", hook_event: "PreToolUse", ...overrides });
}

function toolFinished(overrides) {
  return makeEvent({ event: "tool_finished", hook_event: "PostToolUse", outcome: "success", ...overrides });
}

// --- tests ---

test("job starts on tool_started event with Bash tool", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_started", (e) => events.push(e));

  jc.handleEvent(toolStarted({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:00.000Z",
    command: "cargo",
    agent_id: "agent-1",
    run_id: "run-1",
    phase: "implement",
  }));

  assert.equal(events.length, 1);
  const e = events[0];
  assert.equal(e.event, "job_started");
  assert.equal(e.job_id, "u1");
  assert.equal(e.run_id, "run-1");
  assert.equal(e.agent_id, "agent-1");
  assert.equal(e.command, "cargo");
  assert.equal(e.phase, "implement");
  assert.equal(e.started_at, "2026-08-07T00:00:00.000Z");
  assert.equal(e.status, "running");
  assert.equal(jc.getActiveJobs().size, 1);
});

test("job completes on matching tool_finished event", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_completed", (e) => events.push(e));

  jc.handleEvent(toolStarted({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:00.000Z",
    command: "cargo",
  }));
  jc.handleEvent(toolFinished({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:05.000Z",
    command: "cargo",
  }));

  assert.equal(events.length, 1);
  assert.equal(events[0].event, "job_completed");
  assert.equal(events[0].job_id, "u1");
  assert.equal(events[0].elapsed_ms, 5000);
  assert.equal(events[0].status, "completed");
  assert.equal(jc.getActiveJobs().size, 0);
  assert.equal(jc.getCompletedJobs().length, 1);
});

test("non-Bash tools are ignored", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_started", (e) => events.push(e));

  jc.handleEvent(toolStarted({ tool_name: "Read", tool_use_id: "u-read" }));
  jc.handleEvent(toolStarted({ tool_name: "Edit", tool_use_id: "u-edit" }));
  jc.handleEvent(toolStarted({ tool_name: "Write", tool_use_id: "u-write" }));
  jc.handleEvent(toolStarted({ tool_name: "Agent", tool_use_id: "u-agent" }));

  assert.equal(events.length, 0);
  assert.equal(jc.getActiveJobs().size, 0);
});

test("heartbeat updates lastHeartbeatAt on periodic check", () => {
  const jc = new JobController({ heartbeatIntervalMs: 1000 });
  const events = [];
  jc.on("job_heartbeat", (e) => events.push(e));

  jc.handleEvent(toolStarted({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:00.000Z",
    command: "cargo",
  }));

  // First heartbeat at 1.5s — should fire (>1s interval)
  jc.checkHeartbeats(new Date("2026-08-07T00:00:01.500Z"));
  assert.equal(events.length, 1);
  assert.equal(events[0].event, "job_heartbeat");
  assert.equal(events[0].job_id, "u1");
  assert.equal(events[0].elapsed_ms, 1500);
  assert.equal(events[0].status, "running");

  // Check immediately again — should NOT fire (too soon)
  jc.checkHeartbeats(new Date("2026-08-07T00:00:01.600Z"));
  assert.equal(events.length, 1, "no heartbeat within interval");

  // Check at 2.5s — should fire again
  jc.checkHeartbeats(new Date("2026-08-07T00:00:02.500Z"));
  assert.equal(events.length, 2);

  // Verify lastHeartbeatAt was updated
  const job = jc.getJobStatus("u1");
  assert.equal(job.lastHeartbeatAt, "2026-08-07T00:00:02.500Z");
});

test("timeout detection via checkTimeouts", () => {
  const jc = new JobController({ timeoutMs: 5000 });
  const events = [];
  jc.on("job_timeout", (e) => events.push(e));

  jc.handleEvent(toolStarted({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:00.000Z",
    command: "cargo",
    run_id: "run-1",
    agent_id: "agent-1",
    phase: "implement",
  }));

  // Not yet timed out at 4s
  const early = jc.checkTimeouts(new Date("2026-08-07T00:00:04.000Z"));
  assert.equal(early.length, 0);
  assert.equal(events.length, 0);

  // Timed out at 6s
  const timedOut = jc.checkTimeouts(new Date("2026-08-07T00:00:06.000Z"));
  assert.equal(timedOut.length, 1);
  assert.equal(events.length, 1);
  assert.equal(events[0].event, "job_timeout");
  assert.equal(events[0].job_id, "u1");
  assert.equal(events[0].status, "timeout");
  assert.equal(events[0].elapsed_ms, 6000);
  assert.ok(events[0].reason);
  assert.equal(events[0].reason.type, "timeout");
  assert.equal(events[0].reason.timeout_ms, 5000);

  // Job should no longer be active
  assert.equal(jc.getActiveJobs().size, 0);
  assert.equal(jc.getCompletedJobs().length, 1);
  assert.equal(jc.getCompletedJobs()[0].status, "timeout");
});

test("cancel with structured reason", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_cancelled", (e) => events.push(e));

  jc.handleEvent(toolStarted({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:00.000Z",
    command: "make",
  }));

  const result = jc.cancel("u1", "user requested cancellation");
  assert.ok(result);
  assert.equal(events.length, 1);
  assert.equal(events[0].event, "job_cancelled");
  assert.equal(events[0].status, "cancelled");
  assert.equal(events[0].reason.type, "cancelled");
  assert.equal(events[0].reason.message, "user requested cancellation");
  assert.equal(jc.getActiveJobs().size, 0);
});

test("cancel returns false for unknown job", () => {
  const jc = new JobController();
  assert.equal(jc.cancel("nonexistent", "reason"), false);
});

test("cancel returns false for already-completed job", () => {
  const jc = new JobController();
  jc.handleEvent(toolStarted({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:00.000Z" }));
  jc.handleEvent(toolFinished({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:01.000Z" }));
  assert.equal(jc.cancel("u1", "too late"), false);
});

test("summary aggregation", () => {
  const jc = new JobController();

  // Two jobs: one completed, one timed out
  jc.handleEvent(toolStarted({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:00.000Z", command: "cargo" }));
  jc.handleEvent(toolFinished({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:03.000Z", command: "cargo" }));
  jc.handleEvent(toolStarted({ tool_use_id: "u2", observed_at: "2026-08-07T00:00:04.000Z", command: "npm" }));
  jc.handleEvent(toolFinished({ tool_use_id: "u2", observed_at: "2026-08-07T00:00:06.000Z", command: "npm" }));

  const s = jc.summary();
  assert.equal(s.total_jobs, 2);
  assert.equal(s.completed, 2);
  assert.equal(s.timed_out, 0);
  assert.equal(s.cancelled, 0);
  assert.equal(s.active, 0);
  assert.equal(s.total_wait_ms, 5000);
  assert.deepEqual(s.longest_job, { job_id: "u1", command: "cargo", elapsed_ms: 3000 });
});

test("getJobStatus returns null for unknown job", () => {
  const jc = new JobController();
  assert.equal(jc.getJobStatus("nonexistent"), null);
});

test("getJobStatus returns job state for active job", () => {
  const jc = new JobController();
  jc.handleEvent(toolStarted({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:00.000Z",
    command: "cargo",
    run_id: "run-1",
    agent_id: "agent-1",
    phase: "test",
    task_id: "t226.3",
  }));
  const job = jc.getJobStatus("u1");
  assert.ok(job);
  assert.equal(job.jobId, "u1");
  assert.equal(job.runId, "run-1");
  assert.equal(job.agentId, "agent-1");
  assert.equal(job.command, "cargo");
  assert.equal(job.phase, "test");
  assert.equal(job.taskId, "t226.3");
  assert.equal(job.status, "running");
});

test("lastOutput is truncated to 500 chars max", () => {
  const jc = new JobController();
  jc.handleEvent(toolStarted({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:00.000Z", command: "cargo" }));

  // Simulate setting lastOutput via tool_finished with long output
  const longOutput = "x".repeat(1000);
  jc.handleEvent(toolFinished({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:01.000Z",
    command: "cargo",
    last_output: longOutput,
  }));

  const completed = jc.getCompletedJobs().find((j) => j.jobId === "u1");
  assert.ok(completed);
  assert.ok(completed.lastOutput.length <= 500);
});

test("tool_finished for Bash without prior tool_started is ignored gracefully", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_completed", (e) => events.push(e));

  // No prior start
  jc.handleEvent(toolFinished({ tool_use_id: "u-orphan", observed_at: "2026-08-07T00:00:01.000Z" }));
  assert.equal(events.length, 0);
});

test("failure outcome on tool_finished marks job completed with failure outcome", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_completed", (e) => events.push(e));

  jc.handleEvent(toolStarted({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:00.000Z", command: "cargo" }));
  jc.handleEvent(toolFinished({
    tool_use_id: "u1",
    observed_at: "2026-08-07T00:00:05.000Z",
    command: "cargo",
    hook_event: "PostToolUseFailure",
    outcome: "failure",
  }));

  assert.equal(events.length, 1);
  assert.equal(events[0].status, "completed");
  assert.equal(events[0].outcome, "failure");
});

test("90s+ simulated fixture verifies heartbeat tracking", () => {
  const jc = new JobController({ heartbeatIntervalMs: 30000 });
  const heartbeats = [];
  jc.on("job_heartbeat", (e) => heartbeats.push(e));

  const start = new Date("2026-08-07T00:00:00.000Z");
  jc.handleEvent(toolStarted({
    tool_use_id: "u-long",
    observed_at: start.toISOString(),
    command: "make",
  }));

  // Simulate heartbeat checks every 30s for 90s
  for (let s = 30; s <= 90; s += 30) {
    jc.checkHeartbeats(new Date(start.getTime() + s * 1000));
  }

  assert.equal(heartbeats.length, 3, "should have 3 heartbeats at 30s, 60s, 90s");
  assert.equal(heartbeats[0].elapsed_ms, 30000);
  assert.equal(heartbeats[1].elapsed_ms, 60000);
  assert.equal(heartbeats[2].elapsed_ms, 90000);

  // Complete at 95s
  jc.handleEvent(toolFinished({
    tool_use_id: "u-long",
    observed_at: new Date(start.getTime() + 95000).toISOString(),
    command: "make",
  }));

  const completed = jc.getCompletedJobs();
  assert.equal(completed.length, 1);
  assert.equal(completed[0].elapsedMs, 95000);
  assert.equal(completed[0].status, "completed");
});

test("timeout fixture verifies cleanup and structured output", () => {
  const TIMEOUT = 60000; // 60s timeout
  const jc = new JobController({ timeoutMs: TIMEOUT, heartbeatIntervalMs: 20000 });
  const heartbeats = [];
  const timeouts = [];
  jc.on("job_heartbeat", (e) => heartbeats.push(e));
  jc.on("job_timeout", (e) => timeouts.push(e));

  const start = new Date("2026-08-07T00:00:00.000Z");
  jc.handleEvent(toolStarted({
    tool_use_id: "u-timeout",
    observed_at: start.toISOString(),
    command: "sleep",
    run_id: "run-1",
    agent_id: "agent-1",
    phase: "implement",
  }));

  // Heartbeats at 20s, 40s
  jc.checkHeartbeats(new Date(start.getTime() + 20000));
  jc.checkHeartbeats(new Date(start.getTime() + 40000));
  assert.equal(heartbeats.length, 2);

  // Timeout at 70s
  const timedOut = jc.checkTimeouts(new Date(start.getTime() + 70000));
  assert.equal(timedOut.length, 1);
  assert.equal(timeouts.length, 1);

  const te = timeouts[0];
  assert.equal(te.event, "job_timeout");
  assert.equal(te.job_id, "u-timeout");
  assert.equal(te.status, "timeout");
  assert.equal(te.reason.type, "timeout");
  assert.equal(te.reason.timeout_ms, TIMEOUT);
  assert.ok(te.elapsed_ms >= TIMEOUT, "elapsed should exceed timeout");

  // No active jobs remain
  assert.equal(jc.getActiveJobs().size, 0);

  // Summary reflects the timeout
  const s = jc.summary();
  assert.equal(s.total_jobs, 1);
  assert.equal(s.timed_out, 1);
  assert.equal(s.active, 0);
});

test("no polling loops in the API — all methods are synchronous queries", () => {
  // Verify that the JobController API is synchronous/query-based.
  // None of the public methods should return promises or use timers.
  const jc = new JobController();
  const methods = ["handleEvent", "getActiveJobs", "getJobStatus", "checkTimeouts", "checkHeartbeats", "cancel", "getCompletedJobs", "summary"];
  for (const m of methods) {
    assert.equal(typeof jc[m], "function", `${m} should be a function`);
  }

  // All return values are synchronous (not Promises)
  jc.handleEvent(toolStarted({ tool_use_id: "u-sync", observed_at: new Date().toISOString() }));
  const results = [
    jc.getActiveJobs(),
    jc.getJobStatus("u-sync"),
    jc.checkTimeouts(new Date()),
    jc.checkHeartbeats(new Date()),
    jc.cancel("u-sync", "test"),
    jc.getCompletedJobs(),
    jc.summary(),
  ];
  for (const r of results) {
    assert.ok(!(r instanceof Promise), "API methods must be synchronous, no polling loops");
  }
});

test("multiple concurrent jobs tracked independently", () => {
  const jc = new JobController();
  const started = [];
  const completed = [];
  jc.on("job_started", (e) => started.push(e));
  jc.on("job_completed", (e) => completed.push(e));

  jc.handleEvent(toolStarted({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:00.000Z", command: "cargo", agent_id: "a1" }));
  jc.handleEvent(toolStarted({ tool_use_id: "u2", observed_at: "2026-08-07T00:00:01.000Z", command: "npm", agent_id: "a2" }));

  assert.equal(jc.getActiveJobs().size, 2);
  assert.equal(started.length, 2);

  // Complete u2 first
  jc.handleEvent(toolFinished({ tool_use_id: "u2", observed_at: "2026-08-07T00:00:03.000Z", command: "npm", agent_id: "a2" }));
  assert.equal(jc.getActiveJobs().size, 1);
  assert.ok(jc.getActiveJobs().has("u1"));

  // Complete u1
  jc.handleEvent(toolFinished({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:05.000Z", command: "cargo", agent_id: "a1" }));
  assert.equal(jc.getActiveJobs().size, 0);
  assert.equal(completed.length, 2);
});

test("duplicate tool_started for same tool_use_id is ignored", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_started", (e) => events.push(e));

  jc.handleEvent(toolStarted({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:00.000Z" }));
  jc.handleEvent(toolStarted({ tool_use_id: "u1", observed_at: "2026-08-07T00:00:01.000Z" }));

  assert.equal(events.length, 1, "duplicate start should be ignored");
  assert.equal(jc.getActiveJobs().size, 1);
});

test("tool_started without tool_use_id is ignored", () => {
  const jc = new JobController();
  const events = [];
  jc.on("job_started", (e) => events.push(e));

  jc.handleEvent(toolStarted({ tool_use_id: undefined }));
  assert.equal(events.length, 0);
});
