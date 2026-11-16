// Syntax / contract check for agents/session-closer.md: the workflow launches
// it as 'handoff-task-loop:session-closer', so the definition must exist, have
// parseable frontmatter, and describe each Step 6 duty the Close stage relies on.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENT = join(HERE, '..', '..', 'agents', 'session-closer.md');
const WORKFLOW = join(HERE, '..', 'session-execute.js');

function parse() {
  const text = readFileSync(AGENT, 'utf8');
  const m = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  assert.ok(m, 'agent file must start with a --- frontmatter block');
  const fm = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    assert.ok(kv, `unparseable frontmatter line: ${line}`);
    fm[kv[1]] = kv[2].trim();
  }
  return { fm, body: m[2] };
}

test('frontmatter names the agent, model, and a non-empty tool list', () => {
  const { fm } = parse();
  assert.equal(fm.name, 'session-closer');
  assert.equal(fm.agentType, 'session-closer');
  assert.equal(fm.model, 'sonnet');
  assert.ok(fm.description.length > 0);
  const tools = fm.tools.replace(/^\[|\]$/g, '').split(',').map((t) => t.trim());
  for (const t of ['Read', 'Bash', 'Edit', 'Write']) assert.ok(tools.includes(t), `tools must include ${t}`);
});

test('the workflow launches exactly this agent type', () => {
  const src = readFileSync(WORKFLOW, 'utf8');
  assert.match(src, /agentType: 'handoff-task-loop:session-closer'/);
});

test('the body covers every Step 6 duty and the non-fatal error policy', () => {
  const { body } = parse();
  for (const needle of [
    'handoff_check_criterion',
    'handoff_update_task',
    'handoff_trace_update',
    'done_criteria progress',
    'Requirements addressed',
    'requirement_ids',
    'warnings',
  ]) {
    assert.ok(body.includes(needle), `body must mention ${needle}`);
  }
  assert.match(body, /continue/i);
});

test('the body forbids marking a not-fully-verified task done', () => {
  const { body } = parse();
  assert.match(body, /`review`/);
});

// ------------------------------------------------------------
// Requirement-link rule (REQ-TRC-020, SPEC-TRC-011 / SPEC-TRC-012)
// ------------------------------------------------------------
// A task with no requirement link is closed as `review`, never `done`, with a
// fixed warning appended to its notes. The wording is pinned here because the
// closer, the workflow's inline closer prompt and the verifier must agree.

/** The note the closer appends to a task it leaves in review for this reason. */
const REQUIREMENT_LINK_NOTE =
  'Requirement link missing: this task has no requirement link, so it was left in review instead of done. '
  + 'Link it to the requirement it implements (handoff_update_task requirement_ids; handoff_trace_propose suggests candidates), '
  + 'or add the label no-trace if it is deliberately outside requirement traceability, then mark it done.';

const VERIFIER = join(HERE, '..', '..', 'agents', 'session-closer-verifier.md');
const flat = (text) => text.replace(/\s+/g, ' ');

/** The text of one `### <n>. <title>` step of the closer's procedure. */
function step(body, heading) {
  const start = body.indexOf(heading);
  assert.ok(start >= 0, `the closer must have a step headed ${heading}`);
  const rest = body.slice(start + heading.length);
  const next = rest.search(/\n### |\n## /);
  return body.slice(start, next < 0 ? body.length : start + heading.length + next);
}

/** The inline closer prompt of the workflow (its template literal lines, joined). */
function workflowCloserPrompt() {
  const src = readFileSync(WORKFLOW, 'utf8');
  const start = src.indexOf('function buildCloserPrompt()');
  const end = src.indexOf('function specScopeIsFull()');
  assert.ok(start >= 0 && end > start, 'buildCloserPrompt must exist');
  return src.slice(start, end);
}

test('the closer links requirements before it decides the status', () => {
  const { body } = parse();
  const order = [
    '### 1. Check off done_criteria',
    '### 2. Trace update',
    '### 3. Requirement links',
    '### 4. Set the task status',
    '### 4b. Log actual time',
    '### 5. Update the roadmap',
  ].map((h) => {
    const at = body.indexOf(h);
    assert.ok(at >= 0, `missing step heading: ${h}`);
    return at;
  });
  assert.deepEqual([...order].sort((a, b) => a - b), order, 'the steps must appear in this order');
});

test('the status step checks the requirement link with handoff_get_task include_orphan', () => {
  const s = flat(step(parse().body, '### 4. Set the task status'));
  assert.ok(s.includes('handoff_get_task(task_id, include_orphan: true)'), 'the read call');
  assert.match(s, /`orphan: true`/);
  assert.match(s, /after Steps 2 and 3/);
});

test('an orphan task is set to review, never done, with the fixed note in the same call (AC1, AC2)', () => {
  const { body } = parse();
  const s = flat(step(body, '### 4. Set the task status'));
  assert.match(s, /`orphan: true`[^.]*set the status to `review`, never `done`/);
  assert.ok(s.includes('notes_append'), 'the warning goes in notes_append');
  assert.match(s, /same `handoff_update_task` call/);
  assert.ok(flat(body).includes(REQUIREMENT_LINK_NOTE), 'the fixed note text');
  assert.match(s, /`requirement_link_missing: true`/);
  // `done` needs `orphan` not to be true.
  assert.match(s, /`done` — only when \*\*all\*\* of these hold:[^-]*`orphan` is not `true`/);
});

test('a review the orphan guard rejects is reported as not closed, never forced', () => {
  const s = flat(step(parse().body, '### 4. Set the task status'));
  assert.match(s, /rejected with `orphan_guard: \.\.\.`/);
  assert.match(s, /`\[trace\] orphan_guard = "block"`/);
  assert.match(s, /do not pass `force`/);
  assert.match(s, /only `notes_append`/);
  assert.match(s, /`status_set: "unchanged"`[^.]*`ops_failed`/);
  const p = flat(workflowCloserPrompt().replace(/\\`/g, '`'));
  assert.match(p, /do not pass force/);
  assert.match(p, /notes_append-only/);
});

test('an in_progress task is not turned into a review by the rule', () => {
  const s = flat(step(parse().body, '### 4. Set the task status'));
  assert.match(s, /leave `in_progress` stays `in_progress`, without the note/);
});

test('a no-trace task and a project without a layer document close as before (AC3, AC4)', () => {
  const s = flat(step(parse().body, '### 4. Set the task status'));
  assert.match(s, /labelled `no-trace`[^.]*`orphan: false`[^.]*`done` as usual/);
  assert.match(s, /no layer document[^.]*`orphan: false`/);
  assert.match(s, /never decide this yourself from `task_links` or `labels`/);
});

test('a missing orphan key or a failed check is handled explicitly, never guessed', () => {
  const s = flat(step(parse().body, '### 4. Set the task status'));
  assert.match(s, /no `orphan` key[^.]*older server[^.]*close the task as before/);
  assert.match(s, /call fails[^.]*`review`[^.]*`ops_failed`[^.]*`orphan_check`/);
});

test('the trace step skips the no-requirement line of a developer report', () => {
  const s = flat(step(parse().body, '### 2. Trace update'));
  assert.match(s, /`- None`/);
  assert.match(s, /names no stable_id/);
});

test('the CLI table documents the orphan read', () => {
  const { body } = parse();
  assert.ok(body.includes('handoff-mcp task get --task-id <id> --include-orphan true --project-dir .'));
});

test('the output lists requirement_link_missing and the verifier is said to read orphan back', () => {
  const { body } = parse();
  const out = flat(body.slice(body.indexOf('## Output')));
  assert.ok(out.includes('`requirement_link_missing`'));
  assert.match(flat(body), /`requirement_link_missing`[^.]*`orphan`/);
});

/** The note constant of the workflow: its string literals, concatenated. */
function workflowNote() {
  const src = readFileSync(WORKFLOW, 'utf8');
  const m = src.match(/const REQUIREMENT_LINK_MISSING_NOTE =((?:\s*\+?\s*'[^']*')+);/);
  assert.ok(m, 'the workflow must define REQUIREMENT_LINK_MISSING_NOTE as string literals');
  return [...m[1].matchAll(/'([^']*)'/g)].map((x) => x[1]).join('');
}

test('the workflow closer prompt states the same rule with the same note', () => {
  assert.equal(workflowNote(), REQUIREMENT_LINK_NOTE, 'the workflow and the agent definition must use one wording');
  const p = workflowCloserPrompt();
  // The prompt is built from template-literal lines; compare without the line glue.
  const text = flat(p.replace(/`,\s*\n\s*`/g, ' ').replace(/\\`/g, '`'));
  assert.ok(text.includes('include_orphan: true'), 'the prompt names the read');
  assert.ok(text.includes('"${REQUIREMENT_LINK_MISSING_NOTE}"'), 'the prompt carries the fixed note');
  assert.ok(text.includes('notes_append'), 'the note goes in notes_append');
  assert.ok(text.includes('requirement_link_missing'), 'the prompt names the result field');
  assert.match(text, /never `done`/);
  // Links come before the status in the prompt too.
  assert.ok(text.indexOf('handoff_trace_update') < text.indexOf('include_orphan: true'));
});

test('the closer schema carries requirement_link_missing and the verifier schema orphan, both optional', () => {
  const src = readFileSync(WORKFLOW, 'utf8');
  const closer = src.slice(src.indexOf('const CLOSER_SCHEMA'), src.indexOf('const CLOSER_VERIFIER_SCHEMA'));
  assert.ok(closer.includes("requirement_link_missing: { type: 'boolean' }"));
  assert.ok(closer.includes("required: ['task_id', 'status_set']"));
  const verifier = src.slice(src.indexOf('const CLOSER_VERIFIER_SCHEMA'), src.indexOf('const SPEC_WRITER_SCHEMA'));
  assert.ok(verifier.includes("orphan: { type: 'boolean' }"));
  assert.ok(verifier.includes("required: ['task_id', 'status', 'checked_indices']"));
});

test('the verifier reads orphan back and treats review-for-a-missing-link as an expected state (SPEC-TRC-012 AC3)', () => {
  const text = readFileSync(VERIFIER, 'utf8');
  const f = flat(text);
  assert.ok(f.includes('handoff_get_task(task_id, include_orphan: true)'));
  assert.ok(text.includes('handoff-mcp task get --task-id <id> --include-orphan true --project-dir .'));
  assert.match(f, /`orphan` — the task's `orphan` field, verbatim/);
  assert.match(f, /leave `orphan` out/);
  assert.match(f, /`review`[^.]*no requirement link[^.]*expected state, not a failed close-out/);
  // Still read-only and still no judgement of its own.
  assert.match(f, /never write/i);
});

test('the workflow verifier prompt asks for orphan', () => {
  const src = readFileSync(WORKFLOW, 'utf8');
  const start = src.indexOf('function buildCloserVerifierPrompt');
  const p = src.slice(start, src.indexOf('function buildCloserPrompt()'));
  assert.ok(p.includes('include_orphan'));
  assert.ok(p.includes('orphan'));
  assert.ok(p.includes('--include-orphan true'));
});
