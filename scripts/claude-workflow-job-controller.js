/**
 * Job controller for long-running Bash commands.
 *
 * Receives tool_started/tool_finished events from the observer event stream
 * and maintains a map of active jobs with heartbeat, timeout, and cancel
 * semantics.  It is a library module — it never writes to stdout in normal
 * operation.  Privacy: only command basenames, never full command text.
 *
 * The API is entirely synchronous (query-based).  Workflow code calls
 * checkTimeouts(now) / checkHeartbeats(now) at its own cadence instead of
 * running sleep/poll loops internally.
 */

"use strict";

const MAX_LAST_OUTPUT = 500;

// Minimal synchronous EventEmitter (no node:events dependency assumption).
class Emitter {
  constructor() {
    this._listeners = Object.create(null);
  }
  on(event, fn) {
    (this._listeners[event] ||= []).push(fn);
    return this;
  }
  emit(event, data) {
    for (const fn of this._listeners[event] || []) fn(data);
  }
}

function truncate(text, max) {
  if (typeof text !== "string") return undefined;
  return text.length <= max ? text : text.slice(-max);
}

function parseTime(value) {
  const time = Date.parse(value || "");
  return Number.isFinite(time) ? time : undefined;
}

class JobController extends Emitter {
  /**
   * @param {object} [opts]
   * @param {number} [opts.timeoutMs=300000]      Default 5 minutes
   * @param {number} [opts.heartbeatIntervalMs=30000]  Default 30 seconds
   */
  constructor(opts = {}) {
    super();
    this._timeoutMs = opts.timeoutMs ?? 300000;
    this._heartbeatIntervalMs = opts.heartbeatIntervalMs ?? 30000;
    /** @type {Map<string, object>} active jobs keyed by tool_use_id */
    this._active = new Map();
    /** @type {Array<object>} completed/timed-out/cancelled jobs */
    this._completed = [];
  }

  /**
   * Process a normalized observer event.
   * Only Bash tool events create/complete jobs.
   */
  handleEvent(event) {
    if (!event || typeof event !== "object") return;
    const toolName = event.tool_name;
    if (toolName !== "Bash" && toolName !== undefined) return;
    // Require Bash for job tracking — events without tool_name are not tool events.
    if (!toolName) return;

    const toolUseId = event.tool_use_id;
    if (!toolUseId) return;

    if (event.event === "tool_started") {
      this._onToolStarted(event, toolUseId);
    } else if (event.event === "tool_finished") {
      this._onToolFinished(event, toolUseId);
    }
  }

  _onToolStarted(event, toolUseId) {
    // Ignore duplicate starts
    if (this._active.has(toolUseId)) return;

    const now = event.observed_at;
    const job = {
      jobId: toolUseId,
      runId: event.run_id,
      agentId: event.agent_id,
      command: event.command,
      phase: event.phase,
      taskId: event.task_id,
      startedAt: now,
      lastHeartbeatAt: now,
      lastOutput: undefined,
      status: "running",
      elapsedMs: 0,
      completedAt: undefined,
      reason: undefined,
    };
    this._active.set(toolUseId, job);

    this.emit("job_started", this._makeEvent("job_started", job, now));
  }

  _onToolFinished(event, toolUseId) {
    const job = this._active.get(toolUseId);
    if (!job) return; // orphan finish — ignore gracefully

    const now = event.observed_at;
    const startMs = parseTime(job.startedAt);
    const endMs = parseTime(now);
    job.status = "completed";
    job.completedAt = now;
    job.elapsedMs = startMs != null && endMs != null ? endMs - startMs : 0;
    job.lastOutput = truncate(event.last_output, MAX_LAST_OUTPUT);

    this._active.delete(toolUseId);
    this._completed.push(job);

    const emitted = this._makeEvent("job_completed", job, now);
    if (event.outcome) emitted.outcome = event.outcome;
    this.emit("job_completed", emitted);
  }

  /**
   * Check for timed-out jobs.  Returns an array of timed-out job entries.
   * @param {Date} now
   */
  checkTimeouts(now) {
    const nowMs = now.getTime();
    const timedOut = [];
    for (const [id, job] of this._active) {
      const startMs = parseTime(job.startedAt);
      if (startMs == null) continue;
      const elapsed = nowMs - startMs;
      if (elapsed >= this._timeoutMs) {
        job.status = "timeout";
        job.completedAt = now.toISOString();
        job.elapsedMs = elapsed;
        job.reason = { type: "timeout", timeout_ms: this._timeoutMs };
        this._active.delete(id);
        this._completed.push(job);
        timedOut.push(job);
        this.emit("job_timeout", this._makeEvent("job_timeout", job, now.toISOString()));
      }
    }
    return timedOut;
  }

  /**
   * Emit heartbeat events for long-running jobs.
   * @param {Date} now
   */
  checkHeartbeats(now) {
    const nowMs = now.getTime();
    for (const job of this._active.values()) {
      const lastHbMs = parseTime(job.lastHeartbeatAt);
      if (lastHbMs == null) continue;
      if (nowMs - lastHbMs >= this._heartbeatIntervalMs) {
        job.lastHeartbeatAt = now.toISOString();
        const startMs = parseTime(job.startedAt);
        job.elapsedMs = startMs != null ? nowMs - startMs : 0;
        this.emit("job_heartbeat", this._makeEvent("job_heartbeat", job, now.toISOString()));
      }
    }
  }

  /**
   * Cancel a running job.
   * @param {string} jobId
   * @param {string} reason
   * @returns {boolean} true if cancelled, false if job not found or already finished
   */
  cancel(jobId, reason) {
    const job = this._active.get(jobId);
    if (!job) return false;

    const now = new Date();
    const startMs = parseTime(job.startedAt);
    const nowMs = now.getTime();
    job.status = "cancelled";
    job.completedAt = now.toISOString();
    job.elapsedMs = startMs != null ? nowMs - startMs : 0;
    job.reason = { type: "cancelled", message: reason };
    job.lastOutput = truncate(job.lastOutput, MAX_LAST_OUTPUT);

    this._active.delete(jobId);
    this._completed.push(job);
    this.emit("job_cancelled", this._makeEvent("job_cancelled", job, now.toISOString()));
    return true;
  }

  /** @returns {Map<string, object>} active jobs keyed by tool_use_id */
  getActiveJobs() {
    return new Map(this._active);
  }

  /**
   * @param {string} jobId
   * @returns {object|null}
   */
  getJobStatus(jobId) {
    return this._active.get(jobId) || this._completed.find((j) => j.jobId === jobId) || null;
  }

  /** @returns {Array<object>} completed/timed-out/cancelled jobs */
  getCompletedJobs() {
    return [...this._completed];
  }

  /** Aggregate stats */
  summary() {
    const all = [...this._completed];
    const activeArr = [...this._active.values()];
    const allJobs = [...all, ...activeArr];
    const completed = all.filter((j) => j.status === "completed").length;
    const timedOut = all.filter((j) => j.status === "timeout").length;
    const cancelled = all.filter((j) => j.status === "cancelled").length;
    const totalWaitMs = allJobs.reduce((sum, j) => sum + (j.elapsedMs || 0), 0);
    const longest = allJobs.reduce((best, j) => (!best || (j.elapsedMs || 0) > (best.elapsedMs || 0) ? j : best), null);
    return {
      total_jobs: allJobs.length,
      active: activeArr.length,
      completed,
      timed_out: timedOut,
      cancelled,
      total_wait_ms: totalWaitMs,
      longest_job: longest ? { job_id: longest.jobId, command: longest.command, elapsed_ms: longest.elapsedMs } : null,
    };
  }

  _makeEvent(type, job, observedAt) {
    return {
      event: type,
      job_id: job.jobId,
      run_id: job.runId,
      agent_id: job.agentId,
      command: job.command,
      phase: job.phase,
      task_id: job.taskId,
      started_at: job.startedAt,
      last_heartbeat_at: job.lastHeartbeatAt,
      elapsed_ms: job.elapsedMs,
      status: job.status,
      ...(job.reason ? { reason: job.reason } : {}),
      ...(job.lastOutput !== undefined ? { last_output: job.lastOutput } : {}),
      observed_at: observedAt,
    };
  }
}

export { JobController };
