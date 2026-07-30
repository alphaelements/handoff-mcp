#!/usr/bin/env node
// ============================================================
// codex-adapter-e2e.js — reproducible E2E for runViaCodex (t216)
// ============================================================
// Runs ONE real `codex exec --json` process through the adapter in
// plugin-task-loop/workflows/lib/codex-adapter.js, using a low-side-effect
// task ("count files in this scratch dir and report"), and asserts the
// returned shape + a non-null usage payload for token-cost accounting.
//
// This is opt-in, manual-trigger E2E (touches the real Codex CLI + a real
// API call) — it is NOT wired into `npm run test:js` or any lefthook git
// hook, matching how tests/codex-plugin-install-e2e.sh (also
// Codex-CLI-dependent) is run standalone rather than from the JS unit-test
// glob. Run it explicitly:
//
//   node tests/codex-adapter-e2e.js
//
// Requires: `codex` CLI on PATH, authenticated (`codex login` already done
// per this project's CLAUDE.md environment notes).

import { mkdtempSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execSync } from 'node:child_process';
import { runViaCodex } from '../plugin-task-loop/workflows/lib/codex-adapter.js';

function log(msg) {
  process.stdout.write(`[codex-adapter-e2e] ${msg}\n`);
}

async function main() {
  // A throwaway git working tree, per the spec doc's precondition #6 (codex
  // exec refuses untrusted non-git directories). Created under the OS temp
  // dir, not this repo's tmp/, so the E2E has zero side effects on the real
  // checkout regardless of outcome.
  const workdir = mkdtempSync(join(tmpdir(), 'codex-adapter-e2e-'));
  execSync('git init -q -b main', { cwd: workdir });
  execSync('git config user.email e2e@example.com', { cwd: workdir });
  execSync('git config user.name e2e', { cwd: workdir });
  writeFileSync(join(workdir, 'a.txt'), 'one');
  writeFileSync(join(workdir, 'b.txt'), 'two');
  writeFileSync(join(workdir, 'README.md'), 'scratch\n');
  execSync('git add -A && git commit -q -m init', { cwd: workdir });

  log(`workdir: ${workdir}`);

  const prompt =
    'Count how many .txt files are in the current directory and write a ' +
    "single line to report.md in the form 'Found N .txt files.' Do not " +
    'modify any other file.';

  log('launching codex exec via runViaCodex (this makes a real API call)...');
  const start = Date.now();

  let result;
  try {
    result = await runViaCodex(prompt, {
      cwd: workdir,
      sandbox: 'workspace-write',
      hangTimeoutMs: 5 * 60 * 1000, // generous for a live API round-trip
      onEvent: (evt) => log(`event: ${evt.type}`),
    });
  } catch (err) {
    log(`FAILED: ${err.name || 'Error'} ${err.code || ''}: ${err.message}`);
    process.exitCode = 1;
    return;
  }

  const elapsedMs = Date.now() - start;

  log(`turn.completed after ${elapsedMs}ms`);
  log(`text: ${JSON.stringify(result.text)}`);
  log(`usage: ${JSON.stringify(result.usage)}`);
  log(`logPath: ${result.logPath}`);

  const reportPath = join(workdir, 'report.md');
  let reportContent = null;
  try {
    reportContent = readFileSync(reportPath, 'utf8');
  } catch {
    // handled below
  }

  const checks = [
    ['result.text is non-empty', typeof result.text === 'string' && result.text.trim() !== ''],
    ['result.usage is present (token cost recoverable)', result.usage !== null && typeof result.usage === 'object'],
    ['result.raw contains turn.completed', result.raw.some((e) => e.type === 'turn.completed')],
    ['report.md was written by the real codex process', reportContent !== null],
    ['report.md mentions the correct count (2)', reportContent !== null && /2/.test(reportContent)],
  ];

  let allPass = true;
  for (const [desc, pass] of checks) {
    log(`${pass ? 'PASS' : 'FAIL'}: ${desc}`);
    if (!pass) allPass = false;
  }

  if (reportContent !== null) log(`report.md content: ${JSON.stringify(reportContent.trim())}`);

  rmSync(workdir, { recursive: true, force: true });

  if (!allPass) {
    process.exitCode = 1;
    return;
  }
  log('E2E PASSED');
}

main().catch((err) => {
  log(`UNCAUGHT: ${err.stack || err.message}`);
  process.exitCode = 1;
});
