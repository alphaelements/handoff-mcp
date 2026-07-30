// ============================================================
// codex-adapter — opt-in Codex execution engine for a single agent() call
// ============================================================
// t216: lets a session-loop caller swap ONE `agent()` invocation for a single
// `codex exec --json` process, when explicitly opted in. Everything else
// (Workflow DSL, session-execute.js, the default Claude subagent path) is
// untouched — see plugin-task-loop/workflows/session-execute.js, which does
// not import this file and is not modified by this task.
//
// This is a PLAIN Node ES module, not a workflow-inlined one: it is not run by
// the Workflow sandbox (which forbids import()/require — see lib/task-graph.js
// header) and it is not (yet) called from session-execute.js. Today it is run
// as a standalone Node script/module (see tests/codex-adapter-e2e.js) — it
// spawns and monitors `codex exec` itself via node:child_process, replacing
// what a human/agent operator would otherwise do by hand with Bash
// `run_in_background` + a Monitor NDJSON subscription (the spec doc's
// prescribed manual protocol). Tomorrow, session-execute.js could import and
// call this directly for a Codex-opted-in role once wired — see this file's
// "Wiring" note in the developer report / README.
//
// Design basis (t216 instructions: read, do not re-investigate):
//   - .handoff/docs/_doc.codex-manager-delegation-spec-revised.md (方式A技術仕様)
//   - .handoff/docs/_doc.codex-manager-delegation-poc-results.md (実測)
//   - .handoff/docs/_doc.codex-manager-delegation-impl-plan-revised.md (Step 1)
//
// Interface shape mirrors the Workflow runtime's own convention: `agent()`
// takes (prompt, opts) and returns a string (or an object when a schema is
// requested — see verdict-logic.js / m-20260710-012803-372344). `runViaCodex`
// follows the same (prompt, opts) -> value shape so a future
// `agent(prompt, { engine: 'codex', ... })` dispatcher in session-execute.js
// can delegate to it as a drop-in for the Codex branch, without a shape
// mismatch at the call site. It always resolves an object (never a bare
// string) because, unlike the Claude runtime, the caller here also needs the
// token-usage figures out of `turn.completed` for cost accounting — a bare
// string would have nowhere to carry that.

import { spawn } from 'node:child_process';
import { closeSync, existsSync, mkdtempSync, openSync, readFileSync, writeSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';

/**
 * Default silence timeout (ms) between NDJSON events before the adapter treats
 * the process as hung and kills it. The spec doc's "ハング検知の閾値" section
 * gives an approximate range (単純作業: 2〜3分, 複雑な作業: 10分程度) rather
 * than a single number; the adapter defaults to the conservative end of the
 * simple-task range and lets a caller override it per role/task complexity via
 * `options.hangTimeoutMs`.
 */
const DEFAULT_HANG_TIMEOUT_MS = 3 * 60 * 1000;

/**
 * NDJSON `item.completed` sub-types worth surfacing in the returned text, per
 * the spec doc's "購読すべきNDJSONイベント種別" list. `agent_message` carries
 * the model's own final-answer prose; the others are progress noise the
 * caller does not need once turn.completed has fired.
 *
 * The discriminator field is `item.type` (e.g. "agent_message",
 * "command_execution", "file_change") — confirmed against a real `codex exec
 * --json` run (E2E, tests/codex-adapter-e2e.js). An earlier draft assumed
 * `item.item_type`, which does not exist in the real payload and silently
 * matched nothing, always returning `text: ''`.
 */
const AGENT_MESSAGE_ITEM_TYPE = 'agent_message';

/**
 * Parse one NDJSON line into an event object, or `null` if it is not
 * parseable JSON (a stray non-JSON line must not crash the reader — codex
 * exec's own stderr noise can end up interleaved when 2>&1 is used).
 */
function parseLine(line) {
  const trimmed = line.trim();
  if (trimmed === '') return null;
  try {
    return JSON.parse(trimmed);
  } catch {
    return null;
  }
}

/**
 * Extract the human-readable final answer out of a `turn.completed` event's
 * sibling `item.completed` / `agent_message` events collected along the way.
 * Falls back to an empty string (never undefined/null) so callers can safely
 * concatenate — a run that produced no agent_message still returns a valid,
 * if empty, `text`.
 */
function extractAgentMessage(events) {
  const messages = events
    .filter((e) => e && e.type === 'item.completed' && e.item && e.item.type === AGENT_MESSAGE_ITEM_TYPE)
    .map((e) => (typeof e.item.text === 'string' ? e.item.text : ''))
    .filter((t) => t !== '');
  return messages.length > 0 ? messages[messages.length - 1] : '';
}

/**
 * Does this event signal a Codex-side error? Matches the spec doc's
 * "`error`系イベント" bullet — the exact type name is not pinned in the docs
 * (no error was observed in the PoC runs), so this matches defensively on
 * both a literal `type: "error"` and any type ending in `.error` /
 * `.failed` /`.aborted`, which is the shape Codex uses elsewhere
 * (`turn.completed`, `item.completed`).
 */
function isErrorEvent(evt) {
  if (!evt || typeof evt.type !== 'string') return false;
  return evt.type === 'error' || /\.(error|failed|aborted)$/.test(evt.type);
}

/** Best-effort human-readable message out of an error-shaped event. */
function describeErrorEvent(evt) {
  if (evt.message && typeof evt.message === 'string') return evt.message;
  if (evt.error && typeof evt.error === 'string') return evt.error;
  if (evt.error && typeof evt.error.message === 'string') return evt.error.message;
  return JSON.stringify(evt);
}

/**
 * Error subclass so callers can `err.code === 'CODEX_HANG'` /
 * `'CODEX_ERROR_EVENT'` / `'CODEX_EXIT_NONZERO'` instead of parsing message
 * text. See "エラーハンドリング方針" in the module doc comment at the bottom
 * of this file for what each code means and what the caller should do.
 */
export class CodexAdapterError extends Error {
  constructor(message, code, details) {
    super(message);
    this.name = 'CodexAdapterError';
    this.code = code;
    this.details = details || {};
  }
}

/**
 * Kill a process tree by PID. `SIGTERM` first; the caller decides whether to
 * escalate. Swallows ESRCH (already exited) — killing an already-dead process
 * is not a failure the caller needs to see.
 */
function tryKill(pid, signal = 'SIGTERM') {
  try {
    process.kill(pid, signal);
  } catch (err) {
    if (err && err.code !== 'ESRCH') throw err;
  }
}

/**
 * Run a single prompt through `codex exec --json`, opting one agent() call
 * into the Codex execution engine.
 *
 * @param {string} prompt - The role/task prompt, embedded directly in the
 *   `codex exec` argv (per spec doc's role-prompt template).
 * @param {object} [options]
 * @param {string} [options.cwd] - Working directory. MUST be inside a git
 *   working tree (spec doc precondition #6) — `codex exec` refuses untrusted
 *   non-git directories otherwise. Defaults to `process.cwd()`.
 * @param {string} [options.codexBin='codex'] - Executable to invoke. Override
 *   in tests to point at a fake CLI.
 * @param {string} [options.sandbox='workspace-write'] - `--sandbox` value.
 *   Deliberately NOT allowed to be 'danger-full-access' or the
 *   bypass-approvals flag by this adapter (t216 constraint) — see
 *   assertAllowedSandbox below.
 * @param {number} [options.hangTimeoutMs=DEFAULT_HANG_TIMEOUT_MS] - Silence
 *   budget between NDJSON events before the run is killed as hung.
 * @param {string} [options.logPath] - Where to write the NDJSON log. Defaults
 *   to a fresh file under a freshly `mkdtemp`'d directory in the OS temp dir;
 *   override in tests to inspect the raw log at a known path. The adapter
 *   never deletes this file/directory itself (deliberately — it is the
 *   post-hoc audit trail this whole design exists to preserve); the caller
 *   owns cleanup, same as tests/codex-adapter-e2e.js does for its own workdir.
 * @param {(evt: object) => void} [options.onEvent] - Optional progress
 *   callback invoked for every parsed NDJSON event, mirroring what a Monitor
 *   subscription would surface to a human operator.
 * @returns {Promise<{text: string, usage: object|null, raw: object[], logPath: string}>}
 *   agent()-equivalent value: `text` is the final agent_message (the string
 *   an `agent()` call without a schema would have returned), `usage` is the
 *   `turn.completed.usage` object for cost accounting (or null if absent),
 *   `raw` is every parsed NDJSON event in order (for debugging/inspection),
 *   `logPath` is where the NDJSON was written.
 * @throws {CodexAdapterError} CODEX_HANG, CODEX_ERROR_EVENT, CODEX_EXIT_NONZERO,
 *   CODEX_NO_TURN_COMPLETED, CODEX_BAD_SANDBOX
 */
export async function runViaCodex(prompt, options = {}) {
  if (typeof prompt !== 'string' || prompt.trim() === '') {
    throw new CodexAdapterError('runViaCodex: prompt must be a non-empty string', 'CODEX_BAD_ARGS');
  }

  const cwd = options.cwd || process.cwd();
  const codexBin = options.codexBin || 'codex';
  const sandbox = options.sandbox || 'workspace-write';
  const hangTimeoutMs = options.hangTimeoutMs || DEFAULT_HANG_TIMEOUT_MS;
  const onEvent = typeof options.onEvent === 'function' ? options.onEvent : () => {};

  assertAllowedSandbox(sandbox);

  const logPath = options.logPath || join(mkdtempSync(join(tmpdir(), 'codex-adapter-')), `${randomUUID()}.ndjson`);

  const args = ['exec', '--json', '--sandbox', sandbox, '--cd', cwd, prompt];

  return new Promise((resolve, reject) => {
    // `< /dev/null` (CLAUDE.md global rule, stdin-hang prevention) — spawn
    // with stdio 'ignore' for stdin achieves the same effect without a shell.
    const child = spawn(codexBin, args, {
      cwd,
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    const fd = openSync(logPath, 'a');
    let settled = false;
    let hangTimer = null;
    const events = [];
    let buffer = '';

    const cleanup = () => {
      if (hangTimer) clearTimeout(hangTimer);
      try {
        closeSync(fd);
      } catch {
        // already closed
      }
    };

    const settle = (fn, arg) => {
      if (settled) return;
      settled = true;
      cleanup();
      fn(arg);
    };

    const armHangTimer = () => {
      if (hangTimer) clearTimeout(hangTimer);
      hangTimer = setTimeout(() => {
        tryKill(child.pid, 'SIGTERM');
        settle(
          reject,
          new CodexAdapterError(
            `runViaCodex: no NDJSON event for ${hangTimeoutMs}ms — treating as hung and killing pid ${child.pid}`,
            'CODEX_HANG',
            { logPath, hangTimeoutMs },
          ),
        );
      }, hangTimeoutMs);
    };

    const handleLine = (line) => {
      const evt = parseLine(line);
      if (evt === null) return;
      events.push(evt);
      writeSync(fd, line.endsWith('\n') ? line : `${line}\n`);
      onEvent(evt);
      armHangTimer();

      if (isErrorEvent(evt)) {
        tryKill(child.pid, 'SIGTERM');
        settle(
          reject,
          new CodexAdapterError(`runViaCodex: Codex reported an error event: ${describeErrorEvent(evt)}`, 'CODEX_ERROR_EVENT', {
            logPath,
            event: evt,
          }),
        );
        return;
      }

      if (evt.type === 'turn.completed') {
        settle(resolve, {
          text: extractAgentMessage(events),
          usage: evt.usage || null,
          raw: events,
          logPath,
        });
      }
    };

    const onChunk = (chunk) => {
      buffer += chunk.toString('utf8');
      let idx;
      // eslint-disable-next-line no-cond-assign
      while ((idx = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, idx);
        buffer = buffer.slice(idx + 1);
        handleLine(line);
      }
    };

    child.stdout.on('data', onChunk);
    child.stderr.on('data', () => {
      // stderr is not part of the NDJSON contract; ignored here but the
      // process is still killed on hang/error via the stdout-driven timer.
    });

    child.on('error', (err) => {
      settle(
        reject,
        new CodexAdapterError(`runViaCodex: failed to spawn ${codexBin}: ${err.message}`, 'CODEX_SPAWN_FAILED', {
          logPath,
        }),
      );
    });

    child.on('close', (code) => {
      if (settled) return;
      // Flush any trailing partial line without a terminating newline.
      if (buffer.trim() !== '') {
        handleLine(buffer);
        buffer = '';
      }
      if (settled) return;

      if (code !== 0) {
        settle(
          reject,
          new CodexAdapterError(`runViaCodex: codex exec exited with code ${code} before turn.completed`, 'CODEX_EXIT_NONZERO', {
            logPath,
            exitCode: code,
          }),
        );
        return;
      }

      settle(
        reject,
        new CodexAdapterError('runViaCodex: codex exec exited 0 without emitting turn.completed', 'CODEX_NO_TURN_COMPLETED', {
          logPath,
        }),
      );
    });

    armHangTimer();
  });
}

/**
 * Reject sandbox values this adapter refuses to pass through, per t216's
 * explicit constraint (spec-revised doc + task instructions): never
 * `danger-full-access`, never the approvals-bypass flag equivalent. Thrown
 * eagerly, before spawning, so a misconfigured caller fails fast rather than
 * granting a Codex process broader access than intended.
 */
function assertAllowedSandbox(sandbox) {
  const FORBIDDEN = ['danger-full-access'];
  if (FORBIDDEN.includes(sandbox)) {
    throw new CodexAdapterError(
      `runViaCodex: sandbox ${JSON.stringify(sandbox)} is not permitted by this adapter (t216 constraint). ` +
        `Use 'workspace-write' (default) or 'read-only'.`,
      'CODEX_BAD_SANDBOX',
    );
  }
}

/**
 * Read back an NDJSON log file written by a previous runViaCodex call (or by
 * `codex exec --json ... > file` directly) and re-derive the same
 * `{text, usage, raw}` shape. Useful for offline inspection / tests without
 * re-running the process, and for the E2E harness (tests/codex-adapter-e2e.js)
 * to assert on the log independently of the live run.
 */
export function parseNdjsonLog(logPath) {
  if (!existsSync(logPath)) {
    throw new CodexAdapterError(`parseNdjsonLog: no such file: ${logPath}`, 'CODEX_LOG_NOT_FOUND');
  }
  const content = readFileSync(logPath, 'utf8');
  const events = content
    .split('\n')
    .map((line) => parseLine(line))
    .filter((e) => e !== null);
  const turnCompleted = events.find((e) => e.type === 'turn.completed');
  return {
    text: extractAgentMessage(events),
    usage: turnCompleted ? turnCompleted.usage || null : null,
    raw: events,
    logPath,
  };
}

export { DEFAULT_HANG_TIMEOUT_MS };
