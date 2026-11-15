// Contract check for the security minimum of requirement-loop Phase 2 (SPEC-ONB-051): with
// `[onboarding] security_min_nfr = true` the coordinator must obtain an explicit decision on
// three security items before Gate #2; with the key off or absent Phase 2 follows the steps
// it always had.
//
// The procedure is prose the coordinator follows, so this pins the wording the behaviour
// depends on, across the command and the two reference documents it points at. It does not
// execute the coordinator.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');
const REPO = join(PLUGIN, '..');
const REFERENCES = join('handoff-requirements', 'references');
const command = readFileSync(join(PLUGIN, 'commands', 'requirement-loop.md'), 'utf8');
const nfrChecklist = readFileSync(join(REPO, 'skills', REFERENCES, 'nfr-checklist.md'), 'utf8');
const gateChecklists = readFileSync(join(REPO, 'skills', REFERENCES, 'preflight-checklists.md'), 'utf8');

/** Prose is hard-wrapped; join it so an assertion does not depend on where a line breaks. */
const flat = (text) => text.replace(/\s+/g, ' ');

/** The three items a decision is required for, as every document names them. */
const ITEMS = ['Handling of secrets', 'Validation of external input', 'Dependency updates'];

const BLOCK_HEADING = '#### Security minimum (only when `[onboarding] security_min_nfr = true`)';

function phase2() {
  const start = command.indexOf('\n### Phase 2: Shared contracts (NFR)');
  const end = command.indexOf('\n### Phase 3:', start);
  assert.ok(start !== -1 && end > start, 'Phase 2 must exist and be followed by Phase 3');
  return command.slice(start, end);
}

/** The security-minimum block of Phase 2: from its heading to the Gate #2 lead-in. */
function block() {
  const sec = phase2();
  const start = sec.indexOf(`\n${BLOCK_HEADING}`);
  assert.notEqual(start, -1, 'Phase 2 must have the security minimum block');
  const end = sec.indexOf('\n**Gate #2', start);
  assert.ok(end > start, 'the block must come before Gate #2');
  return sec.slice(start, end);
}

/** Phase 2 as it reads when the block is skipped. */
function phase2WithoutBlock() {
  const sec = phase2();
  return sec.replace(block(), '');
}

function gate2to3() {
  const start = gateChecklists.indexOf('\n## Gate 2 to 3:');
  const end = gateChecklists.indexOf('\n## Gate 3 to 4:', start);
  assert.ok(start !== -1 && end > start, 'the Gate 2 to 3 checklist must exist');
  return gateChecklists.slice(start, end);
}

// --- AC3: off or absent, Phase 2 is what it was ---

test('the block sits after step 4 and before Gate #2, so steps 1-4 keep their order and numbers', () => {
  const sec = phase2();
  const order = [
    '\n1. Present the **NFR checklist**',
    '\n2. For **each** characteristic, ask the user whether it applies.',
    '\n3. Write the `NFR-*` items into one requirement-layer document',
    '\n4. `handoff_trace_lint(action="nfr_check")`',
    `\n${BLOCK_HEADING}`,
    '\n**Gate #2 — preflight "Gate 2 to 3"** (shared contracts complete).',
    '\nSave `reqflow_state` (`nfr_doc_id`, `completed_phases: [0,1,2]`, `current_phase: 3`).',
  ].map((needle) => sec.indexOf(needle));
  assert.ok(order.every((i) => i !== -1), `every part of Phase 2 must exist, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order);
});

test('AC3: with the key off or absent the block is skipped whole and Phase 2 is steps 1-4 and the gate', () => {
  const sec = flat(block());
  assert.match(sec, /handoff_get_config/);
  assert.match(sec, /onboarding\.security_min_nfr/);
  assert.match(sec, /absent or `false`: skip this block/);
  assert.match(sec, /Ask nothing, record nothing/);
  assert.match(sec, /go straight to Gate #2/);
  assert.match(sec, /exactly steps 1-4 above/);
});

test('AC3: nothing outside the block depends on the key', () => {
  const rest = phase2WithoutBlock();
  for (const needle of ['security_min_nfr', 'Security minimum', 'NFR-SEC-', ...ITEMS]) {
    assert.ok(!rest.includes(needle), `Phase 2 outside the block must not mention "${needle}"`);
  }
  // The rest of the command does not act on the key either.
  const outsidePhase2 = command.replace(phase2(), '');
  assert.ok(!outsidePhase2.includes('security_min_nfr'));
});

// --- the three decisions ---

/** First-column cells of the Markdown table of `text` whose header row starts with `header`. */
function firstColumn(text, header) {
  const lines = text.split('\n').map((line) => line.trim());
  const start = lines.indexOf(header);
  assert.notEqual(start, -1, `a table headed "${header}" must exist`);
  const cells = [];
  for (const line of lines.slice(start + 2)) {
    if (!line.startsWith('|')) break;
    cells.push(line.split('|')[1].trim());
  }
  return cells;
}

test('the block names the three items and asks for one decision each, from the user', () => {
  // The list of items itself, not the example table further down.
  assert.deepEqual(firstColumn(block(), '| Item | What the decision is about |'), ITEMS);
  const sec = flat(block());
  // One call for the three questions: the coordinator asks in batches.
  assert.match(sec, /in one `AskUserQuestion` call, one question per item/);
  assert.match(sec, /"File as a requirement \/ Out of scope"/);
  assert.match(sec, /Do not decide for the user/);
  assert.match(sec, /no answer is not a decision/);
});

test('AC1: a filed item becomes an NFR- item of the NFR document, under the rules of step 3', () => {
  const sec = flat(block());
  assert.match(sec, /\*\*File as a requirement\*\*/);
  assert.match(sec, /`NFR-SEC-nnn`/);
  assert.match(sec, /`nfr_doc_id`/);
  // The block has a numbered list of its own: steps of Phase 2 are named as such.
  assert.match(sec, /under the rules of Phase 2 step 3/);
  assert.match(sec, /run the two `handoff_trace_lint` calls of Phase 2 step 4 again/);
});

test('AC1: an item ruled out leaves its reason in the NFR document and a record in nfr_check', () => {
  const sec = flat(block());
  assert.match(sec, /\*\*Out of scope\*\*/);
  assert.match(sec, /ask the user for the reason/);
  assert.match(sec, /An item without a reason is still undecided/);
  assert.match(sec, /`## Security minimum`/);
  assert.match(sec, /handoff_trace_lint\(action="nfr_check", review=\{"security": "applicable"\}\)/);
  assert.match(sec, /handoff_trace_lint\(action="nfr_check", review=\{"security": "not_applicable"\}\)/);
  // The tool stores a status, not the reason: the reason has to be in the document.
  assert.match(sec, /records the status only/);
});

test('the decision table of the NFR document has one row per item and three allowed decisions', () => {
  const raw = block();
  const table = raw.match(/```markdown\n\s*## Security minimum\n[\s\S]*?```/);
  assert.ok(table, 'the block must show the `## Security minimum` table');
  assert.deepEqual(firstColumn(table[0], '| Item | Decision | Requirement or reason |'), ITEMS);
  const sec = flat(raw);
  assert.match(sec, /`requirement`/);
  assert.match(sec, /`out of scope`/);
  assert.match(sec, /`undecided`/);
});

test('AC2: while an item is undecided the gate is not asked and its checklist is unmet', () => {
  const sec = flat(block());
  assert.match(sec, /\*\*Do not ask Gate #2 while any of the three is `undecided`\.\*\*/);
  assert.match(sec, /the "Gate 2 to 3" checklist is not met/);
  assert.match(sec, /Suspend/);
  // The checklist the gate runs carries the item.
  const gate = flat(gate2to3());
  assert.match(gate, /- \[ \] \(Only when `\[onboarding\] security_min_nfr = true`\.\)/);
  for (const item of ITEMS) {
    assert.ok(gate.toLowerCase().includes(item.toLowerCase()), `the gate checklist must name "${item}"`);
  }
  assert.match(gate, /an `NFR-` item or an "out of scope" reason/);
  assert.match(gate, /An undecided item fails this gate/);
});

test('Gate #2 shows the decisions to the user', () => {
  const sec = flat(block());
  assert.match(sec, /At Gate #2, show the `## Security minimum` table together with the eight-category table/);
});

// --- reference documents ---

test('the gate checklist keeps its four checks; the security item is an addition', () => {
  const gate = gate2to3();
  for (const check of [
    '- [ ] The NFR checklist ([nfr-checklist.md](nfr-checklist.md)) was walked for all eight characteristics',
    '- [ ] An NFR item exists for every applicable concern.',
    '- [ ] Cross-cutting quality targets',
    '- [ ] Each NFR has `- method: manual` or `- method: auto` and an acceptance criterion.',
  ]) {
    assert.ok(gate.includes(check), `the Gate 2 to 3 checklist lost: ${check}`);
  }
  assert.equal((gate.match(/^- \[ \] /gm) || []).length, 5);
});

test('nfr-checklist.md explains the three items, with questions and both outcomes', () => {
  const start = nfrChecklist.indexOf('\n## Security minimum: three decisions');
  assert.notEqual(start, -1, 'nfr-checklist.md must have the security minimum section');
  // The section's own example contains a `## ` line, so it ends at the next real section.
  const end = nfrChecklist.indexOf('\n## Completion check', start);
  assert.ok(end > start);
  const raw = nfrChecklist.slice(start, end);
  const sec = flat(raw);
  assert.match(sec, /`\[onboarding\] security_min_nfr = true`/);
  // Both tables of the section: the questions per item, and the example of the record.
  assert.deepEqual(firstColumn(raw, '| Item | Questions |'), ITEMS);
  assert.deepEqual(firstColumn(raw, '| Item | Decision | Requirement or reason |'), ITEMS);
  assert.match(sec, /`NFR-SEC-nnn`/);
  assert.match(sec, /out of scope/);
  assert.match(sec, /reason/);
  assert.match(sec, /`## Security minimum`/);
  assert.match(sec, /Without the key/);
  // The section sits between the eight characteristics and the completion check.
  const eight = nfrChecklist.indexOf('\n### 8. Functional suitability');
  const completion = nfrChecklist.indexOf('\n## Completion check');
  assert.ok(eight !== -1 && eight < start && start < completion);
});

test('nfr-checklist.md completion check covers the three decisions', () => {
  const completion = flat(nfrChecklist.slice(nfrChecklist.indexOf('\n## Completion check')));
  assert.match(completion, /`\[onboarding\] security_min_nfr = true`/);
  assert.match(completion, /each of the three security minimum items has a decision/);
});

test('the reference documents shipped with the plugin are the ones edited here', () => {
  for (const name of ['nfr-checklist.md', 'preflight-checklists.md']) {
    const source = readFileSync(join(REPO, 'skills', REFERENCES, name), 'utf8');
    const shipped = readFileSync(join(REPO, 'plugin', 'skills', REFERENCES, name), 'utf8');
    assert.equal(shipped, source, `plugin/skills copy of ${name} is out of sync`);
  }
});
