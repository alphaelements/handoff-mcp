#!/usr/bin/env node
/**
 * Privacy-conscious Claude Code workflow observer.
 *
 * Invoked by optional Claude Code command hooks.  It normalizes lifecycle
 * events into append-only JSONL; it never writes to stdout, so observation
 * cannot alter a Claude session.  Prompt text, tool input, and command bodies
 * are intentionally excluded from the event stream.
 *
 * Usage:
 *   HANDOFF_OBSERVER_LOG=/absolute/path/events.jsonl \
 *     scripts/claude-workflow-observer.js < hook-input.json
 *   scripts/claude-workflow-observer.js summarize events.jsonl
 */

"use strict";

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCHEMA_VERSION = 1;
const EVENT_BY_HOOK = {
  SessionStart: "agent_started",
  SessionEnd: "agent_finished",
  SubagentStart: "agent_started",
  SubagentStop: "agent_finished",
  UserPromptSubmit: "phase_changed",
  PreToolUse: "tool_started",
  PostToolUse: "tool_finished",
  PostToolUseFailure: "tool_finished",
};

function string(value) {
  return typeof value === "string" && value ? value : undefined;
}

function commandName(toolName, toolInput) {
  if (toolName !== "Bash" || !toolInput || typeof toolInput !== "object") return undefined;
  const command = string(toolInput.command);
  if (!command) return undefined;
  // Keep an executable identity for duration aggregation, but never the body.
  const first = command.trim().replace(/^\w+=\S+\s+/, "").split(/[\s|;&]+/, 1)[0];
  return first ? path.basename(first) : undefined;
}

function correlation(input) {
  const sessionId = string(input.session_id) || string(input.sessionId);
  const agentId = string(input.agent_id) || string(input.agentId) || sessionId;
  const toolUseId = string(input.tool_use_id) || string(input.toolUseId);
  return {
    agent_id: agentId,
    parent_run_id: string(input.parent_session_id) || string(input.parentSessionId),
    run_id: string(process.env.HANDOFF_WORKFLOW_RUN_ID) || sessionId,
    session_id: sessionId,
    task_id: string(process.env.HANDOFF_WORKFLOW_TASK_ID) || string(input.task_id),
    phase: string(process.env.HANDOFF_WORKFLOW_PHASE) || string(input.phase),
    tool_use_id: toolUseId,
  };
}

function normalize(input, now = new Date()) {
  const hook = string(input.hook_event_name) || string(input.hookEventName);
  const eventType = EVENT_BY_HOOK[hook];
  if (!eventType) return undefined;
  const toolName = string(input.tool_name) || string(input.toolName);
  const event = {
    schema_version: SCHEMA_VERSION,
    event: eventType,
    observed_at: now.toISOString(),
    hook_event: hook,
    ...correlation(input),
  };
  if (toolName) event.tool_name = toolName;
  const executable = commandName(toolName, input.tool_input || input.toolInput);
  if (executable) event.command = executable;
  if (hook === "PostToolUseFailure") event.outcome = "failure";
  if (hook === "PostToolUse") event.outcome = "success";
  if (eventType === "phase_changed") {
    event.phase = event.phase || "prompt";
    event.wait_reason = "model_wait";
  }
  return Object.fromEntries(Object.entries(event).filter(([, value]) => value !== undefined));
}

function appendEvent(event, logPath) {
  if (!logPath) return false;
  fs.mkdirSync(path.dirname(logPath), { recursive: true, mode: 0o700 });
  fs.appendFileSync(logPath, `${JSON.stringify(event)}\n`, { encoding: "utf8", mode: 0o600 });
  return true;
}

function eventKey(event) {
  return event.tool_use_id || `${event.agent_id || event.session_id || "unknown"}:${event.tool_name || "tool"}`;
}

function parseTime(value) {
  const time = Date.parse(value || "");
  return Number.isFinite(time) ? time : undefined;
}

function summarize(events) {
  const starts = new Map();
  const runs = new Map();
  for (const event of events) {
    const runId = event.run_id || event.session_id || event.agent_id || "unknown";
    if (!runs.has(runId)) runs.set(runId, { run_id: runId, events: [], tool_wait_ms: 0, model_wait_ms: 0, tool_count: 0, turn_count: 0, commands: {} });
    const run = runs.get(runId);
    run.events.push(event);
    if (event.event === "phase_changed") run.turn_count += 1;
    if (event.event === "tool_started") {
      run.tool_count += 1;
      starts.set(`${runId}:${eventKey(event)}`, event);
    }
    if (event.event === "tool_finished") {
      const started = starts.get(`${runId}:${eventKey(event)}`);
      const duration = started && parseTime(event.observed_at) - parseTime(started.observed_at);
      if (Number.isFinite(duration) && duration >= 0) {
        run.tool_wait_ms += duration;
        const command = event.command || started.command;
        if (command) run.commands[command] = (run.commands[command] || 0) + duration;
      }
    }
  }
  return [...runs.values()].map((run) => {
    const times = run.events.map((event) => parseTime(event.observed_at)).filter((value) => value !== undefined);
    const elapsed = times.length ? Math.max(...times) - Math.min(...times) : 0;
    run.model_wait_ms = Math.max(0, elapsed - run.tool_wait_ms);
    const longest = Object.entries(run.commands).sort((a, b) => b[1] - a[1])[0];
    const active = new Set();
    const aliases = new Map();
    for (const event of run.events) {
      if (event.event === "agent_started") {
        const keys = [event.agent_id, event.session_id].filter(Boolean);
        const canonical = keys[0];
        if (canonical) {
          active.add(canonical);
          for (const k of keys) aliases.set(k, canonical);
        }
      }
      if (event.event === "agent_finished") {
        const keys = [event.agent_id, event.session_id].filter(Boolean);
        const canonical = keys.map((k) => aliases.get(k)).find(Boolean) || keys[0];
        if (canonical) active.delete(canonical);
      }
    }
    const hasFinished = run.events.some((event) => event.event === "agent_finished");
    return {
      run_id: run.run_id,
      status: hasFinished && active.size === 0 ? "finished" : "running",
      turn_count: run.turn_count,
      tool_count: run.tool_count,
      tool_wait_ms: run.tool_wait_ms,
      model_wait_ms_estimate: run.model_wait_ms,
      command_durations_ms: run.commands,
      longest_command: longest && { command: longest[0], duration_ms: longest[1] },
    };
  });
}

function main(argv, stdin) {
  if (argv[0] === "summarize") {
    const contents = fs.readFileSync(argv[1], "utf8");
    const events = contents.split("\n").filter(Boolean).map((line) => JSON.parse(line));
    process.stdout.write(`${JSON.stringify(summarize(events), null, 2)}\n`);
    return 0;
  }
  let input;
  try { input = JSON.parse(stdin || "{}"); } catch { return 0; }
  const event = normalize(input);
  if (!event) return 0;
  appendEvent(event, process.env.HANDOFF_OBSERVER_LOG);
  return 0;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    main(process.argv.slice(2), fs.readFileSync(0, "utf8"));
  } catch {
    // Hooks are observers, never a reason to interrupt the developer's turn.
    process.exitCode = 0;
  }
}

export { commandName, normalize, summarize };
