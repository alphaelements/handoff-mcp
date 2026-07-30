import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, chmodSync, readFileSync, existsSync, rmSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { runViaCodex, parseNdjsonLog, CodexAdapterError } from './codex-adapter.js';

// This file's fake-bin fixtures live under `codex-adapter-test-*`; the
// adapter's own default NDJSON-log tempdirs (never deleted by the adapter
// itself, by design — see codex-adapter.js's `options.logPath` doc comment)
// live under `codex-adapter-*`. Swept by prefix in one `after` hook rather
// than tracked individually, so this stays correct even for a test that
// exercises the adapter's default (un-overridden) logPath.

// ============================================================
// Test harness: a fake `codex` executable that prints canned NDJSON lines to
// stdout (optionally with real delays), so these tests never touch the real
// network/API and stay fast + deterministic. Each fake is a tiny Node script
// invoked exactly like the real CLI would be: `codex exec --json --sandbox
// <s> --cd <dir> "<prompt>"`.
// ============================================================

// runViaCodex spawns `codexBin` with argv `['exec','--json',...]`. Rather than
// wrap `node <fake.js>` behind a shim, the fake itself is a shebang'd,
// executable JS file (`chmodSync 0o755` + `#!/usr/bin/env node`), so it can be
// passed straight through as `options.codexBin` with no extra indirection.
function fakeCodexBin(dir, script) {
  const path = join(dir, 'fake-codex');
  writeFileSync(path, `#!/usr/bin/env node\n${script}\n`);
  chmodSync(path, 0o755);
  return path;
}

function tmpDir() {
  return mkdtempSync(join(tmpdir(), 'codex-adapter-test-'));
}

after(() => {
  for (const name of readdirSync(tmpdir())) {
    if (name.startsWith('codex-adapter-')) {
      rmSync(join(tmpdir(), name), { recursive: true, force: true });
    }
  }
});

// ============================================================
// Happy path: turn.completed with usage is the success signal
// ============================================================
test('happy path: resolves text + usage once turn.completed appears', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'thread.started' }));
    console.log(JSON.stringify({ type: 'turn.started' }));
    console.log(JSON.stringify({ type: 'item.started', item: { type: 'command_execution' } }));
    console.log(JSON.stringify({ type: 'item.completed', item: { type: 'command_execution' } }));
    console.log(JSON.stringify({ type: 'item.completed', item: { type: 'agent_message', text: 'Found 3 files.' } }));
    console.log(JSON.stringify({ type: 'turn.completed', usage: { input_tokens: 100, output_tokens: 20 } }));
    `,
  );

  const result = await runViaCodex('count the files', { codexBin: bin, cwd: dir });

  assert.equal(result.text, 'Found 3 files.');
  assert.deepEqual(result.usage, { input_tokens: 100, output_tokens: 20 });
  assert.ok(Array.isArray(result.raw) && result.raw.length === 6);
  assert.ok(existsSync(result.logPath), 'NDJSON log must be written to disk');
});

test('happy path: the NDJSON log on disk matches what was parsed', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'item.completed', item: { type: 'agent_message', text: 'ok' } }));
    console.log(JSON.stringify({ type: 'turn.completed', usage: { input_tokens: 1 } }));
    `,
  );

  const result = await runViaCodex('do a thing', { codexBin: bin, cwd: dir });
  const reparsed = parseNdjsonLog(result.logPath);

  assert.equal(reparsed.text, 'ok');
  assert.deepEqual(reparsed.usage, { input_tokens: 1 });
});

// ============================================================
// Completion is decided by turn.completed, NOT process exit alone
// (spec doc: "run_in_background の completed 通知だけに頼らない")
// ============================================================
test('a clean exit (code 0) with no turn.completed is an error, not a silent pass', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'thread.started' }));
    process.exit(0);
    `,
  );

  await assert.rejects(
    () => runViaCodex('do a thing', { codexBin: bin, cwd: dir }),
    (err) => {
      assert.ok(err instanceof CodexAdapterError);
      assert.equal(err.code, 'CODEX_NO_TURN_COMPLETED');
      return true;
    },
  );
});

// ============================================================
// Error handling — (b) error-shaped NDJSON events during the run
// ============================================================
test('an error event mid-stream rejects with CODEX_ERROR_EVENT and kills the process', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'thread.started' }));
    console.log(JSON.stringify({ type: 'error', message: 'sandbox denied write' }));
    // Should never be reached in spirit — the adapter kills us — but keep the
    // fake alive a moment to prove the adapter does not merely wait for exit.
    setTimeout(() => process.exit(1), 2000);
    `,
  );

  await assert.rejects(
    () => runViaCodex('do a thing', { codexBin: bin, cwd: dir }),
    (err) => {
      assert.ok(err instanceof CodexAdapterError);
      assert.equal(err.code, 'CODEX_ERROR_EVENT');
      assert.match(err.message, /sandbox denied write/);
      return true;
    },
  );
});

test('a non-zero exit before turn.completed rejects with CODEX_EXIT_NONZERO', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'thread.started' }));
    process.exit(1);
    `,
  );

  await assert.rejects(
    () => runViaCodex('do a thing', { codexBin: bin, cwd: dir }),
    (err) => {
      assert.equal(err.code, 'CODEX_EXIT_NONZERO');
      assert.equal(err.details.exitCode, 1);
      return true;
    },
  );
});

// ============================================================
// Error handling — (a) hang detection via silence timeout
// ============================================================
test('a silent process is killed and rejected as CODEX_HANG once the timeout elapses', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'thread.started' }));
    // Then go silent forever (simulates a hang) — the test's short
    // hangTimeoutMs must fire well before this process would exit on its own.
    setTimeout(() => {}, 60000);
    `,
  );

  const start = Date.now();
  await assert.rejects(
    () => runViaCodex('do a thing', { codexBin: bin, cwd: dir, hangTimeoutMs: 200 }),
    (err) => {
      assert.equal(err.code, 'CODEX_HANG');
      return true;
    },
  );
  const elapsed = Date.now() - start;
  assert.ok(elapsed < 5000, `hang detection must not wait for the fake's 60s timer (took ${elapsed}ms)`);
});

// ============================================================
// Error handling — (c) a means of killing the process is exercised
// ============================================================
test('the hung child process is actually terminated, not just abandoned', async () => {
  const dir = tmpDir();
  const markerPath = join(dir, 'still-alive-marker');
  const bin = fakeCodexBin(
    dir,
    `
    const fs = require('fs');
    console.log(JSON.stringify({ type: 'thread.started' }));
    // Prove we were killed rather than exiting cleanly: only write the marker
    // on receipt of SIGTERM. If the adapter merely gave up on us, the marker
    // never appears and the process lingers for the full 60s.
    process.on('SIGTERM', () => { fs.writeFileSync(${JSON.stringify(markerPath)}, 'killed'); process.exit(0); });
    setTimeout(() => {}, 60000);
    `,
  );

  await assert.rejects(() => runViaCodex('do a thing', { codexBin: bin, cwd: dir, hangTimeoutMs: 200 }));

  // Give the SIGTERM handler a moment to run and write the marker.
  await new Promise((r) => setTimeout(r, 500));
  assert.ok(existsSync(markerPath), 'adapter must SIGTERM the hung child, not merely stop listening');
});

// ============================================================
// Malformed input guards
// ============================================================
test('an empty prompt is rejected before spawning anything', async () => {
  await assert.rejects(
    () => runViaCodex('', {}),
    (err) => {
      assert.equal(err.code, 'CODEX_BAD_ARGS');
      return true;
    },
  );
});

test('non-JSON stray lines interleaved in stdout are skipped, not fatal', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log('warning: some stderr-ish noise on stdout');
    console.log(JSON.stringify({ type: 'item.completed', item: { type: 'agent_message', text: 'done' } }));
    console.log(JSON.stringify({ type: 'turn.completed', usage: { input_tokens: 5 } }));
    `,
  );

  const result = await runViaCodex('do a thing', { codexBin: bin, cwd: dir });
  assert.equal(result.text, 'done');
});

// ============================================================
// Sandbox constraint (t216: never full-access, never bypass)
// ============================================================
test('danger-full-access sandbox is rejected before spawning', async () => {
  await assert.rejects(
    () => runViaCodex('do a thing', { sandbox: 'danger-full-access' }),
    (err) => {
      assert.equal(err.code, 'CODEX_BAD_SANDBOX');
      return true;
    },
  );
});

test('workspace-write (the default) is accepted', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'turn.completed', usage: {} }));
    `,
  );
  const result = await runViaCodex('do a thing', { codexBin: bin, cwd: dir });
  assert.equal(result.text, '');
});

// ============================================================
// onEvent progress callback (Monitor-equivalent for a Node caller)
// ============================================================
test('onEvent is invoked once per parsed NDJSON event, in order', async () => {
  const dir = tmpDir();
  const bin = fakeCodexBin(
    dir,
    `
    console.log(JSON.stringify({ type: 'thread.started' }));
    console.log(JSON.stringify({ type: 'turn.completed', usage: {} }));
    `,
  );

  const seen = [];
  await runViaCodex('do a thing', { codexBin: bin, cwd: dir, onEvent: (evt) => seen.push(evt.type) });

  assert.deepEqual(seen, ['thread.started', 'turn.completed']);
});
