// Contract check for skills/spec-execute/SKILL.md Phase 3 (REQ-SL-005), Phase 4 (REQ-SL-006)
// and the Mermaid notation rules (REQ-SL-010): the skill is the coordinator's only detailed
// procedure for those phases, so the load-bearing instructions must stay in it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const SKILL = readFileSync(join(HERE, '..', '..', 'skills', 'spec-execute', 'SKILL.md'), 'utf8');

// Split Markdown into chunks that start at a heading of the given prefix ("## " or "### "),
// ignoring heading-looking lines inside fenced code blocks.
function chunks(text, prefix) {
  const out = [];
  let fence = false;
  for (const line of text.split('\n')) {
    if (/^\s*(```|~~~)/.test(line)) fence = !fence;
    if (!fence && line.startsWith(prefix)) out.push([]);
    if (out.length) out[out.length - 1].push(line);
  }
  return out.map((c) => c.join('\n'));
}

// Body of the `## ` section whose heading matches `re`, up to the next `## `.
function section(re) {
  const hit = chunks(SKILL, '## ').find((p) => re.test(p.split('\n', 1)[0]));
  assert.ok(hit, `SKILL.md must have a section matching ${re}`);
  return hit;
}

function subsection(sectionText, re) {
  const hit = chunks(sectionText, '### ').find((p) => re.test(p.split('\n', 1)[0]));
  assert.ok(hit, `section must have a ### subsection matching ${re}`);
  return hit;
}

function mentionsAll(text, needles, where) {
  for (const n of needles) assert.ok(text.includes(n), `${where} must mention ${n}`);
}

test('frontmatter is intact', () => {
  assert.match(SKILL, /^---\nname: spec-execute\ndescription: .+\n---\n/);
});

test('Phase 3: states agent brief, inputs from Phase 1/2 and the gate', () => {
  const s = section(/Phase 3/);
  mentionsAll(s, ['spec-usecase-analyst', 'scope_items', 'architecture', 'screens', 'AskUserQuestion', 'handoff_doc_save'], 'Phase 3');
});

test('Phase 3: use case template lists actor, preconditions, main/alternative scenarios and exceptions', () => {
  const s = section(/Phase 3/);
  mentionsAll(s, ['UC-', 'actor', 'preconditions', 'postconditions', 'main scenario', 'alternative scenarios', 'exceptions', 'Realizes: BS-'], 'Phase 3 template');
});

test('Phase 3: coverage check covers missing, duplicate and unused-screen findings', () => {
  const s = section(/Phase 3/);
  mentionsAll(s, ['missing', 'duplicate', 'every screen'], 'Phase 3 coverage check');
});

test('Phase 3: sequenceDiagram guidelines (participants, alt/opt, size limits, validation)', () => {
  const s = section(/Phase 3/);
  mentionsAll(s, ['sequenceDiagram', 'participant', 'actor', 'alt', 'opt', 'mmdc', 'autonumber'], 'Phase 3 sequenceDiagram guideline');
});

test('Phase 3: gate offers approve / fix gaps / suspend and saves the draft with the spec doc id', () => {
  const s = section(/Phase 3/);
  mentionsAll(s, ['Approve', 'Suspend', 'doc_id', 'phase_done=3'], 'Phase 3 gate');
});

test('Phase 4: assembles from Phases 1-3 and saves with doc_type spec and the required tags', () => {
  const s = section(/Phase 4/);
  mentionsAll(s, ['handoff_doc_save', 'doc_type="spec"', '"spec-loop"', '"requirements-spec"', 'doc_id'], 'Phase 4 save');
});

test('Phase 4: exports with handoff_doc_export(format="pdf", template="spec") and relays warnings', () => {
  const s = section(/Phase 4/);
  assert.match(s, /handoff_doc_export\(doc_id[^)]*format="pdf"[^)]*template="spec"/);
  mentionsAll(s, ['warnings', 'pdf_path', 'Typst'], 'Phase 4 export');
});

test('Phase 4: final gate lists Approve / Revise / Suspend / Stop with their effects', () => {
  const gate = subsection(section(/Phase 4/), /Final gate/);
  mentionsAll(gate, ['Approve', 'Revise', 'Suspend', 'Stop', 'AskUserQuestion'], 'final gate');
  // Revise routes to the owning phase
  mentionsAll(gate, ['Phase 0', 'Phase 1', 'Phase 2', 'Phase 3'], 'Revise routing');
});

test('Phase 4: requirement-loop hand-off marks the doc approved and tells requirement-loop what to read', () => {
  const h = subsection(section(/Phase 4/), /requirement-loop/);
  mentionsAll(h, ['approved', 'handoff_save_context', 'requirement-loop', 'Phase 0', 'refines', 'BS-', 'spec_doc='], 'hand-off');
});

test('Mermaid notation: every REQ-SL-010 type is listed with its phase', () => {
  const s = section(/Mermaid/);
  mentionsAll(s, ['flowchart', 'sequenceDiagram', 'stateDiagram-v2', 'C4Context', 'erDiagram', 'classDiagram', 'mmdc'], 'Mermaid section');
});

test('Phase 4 data-model diagrams: erDiagram / classDiagram are allowed in the architecture section', () => {
  const s = section(/Phase 4/);
  mentionsAll(s, ['erDiagram', 'classDiagram'], 'Phase 4 layout');
});

test('document model: one spec doc, created at the Phase 3 gate, never the Phase 1/2 intermediate docs', () => {
  const phase1 = section(/Phase 1/);
  const gate3 = subsection(section(/Phase 3/), /Gate/);
  const phase4 = section(/Phase 4/);
  // Phase 1 saves an intermediate design doc, not the spec.
  assert.match(phase1, /slug="spec-<topic-slug>-architecture"[^)]*doc_type="design"/);
  // The Phase 3 gate must not claim the spec doc already exists from Phase 1 ...
  assert.doesNotMatch(gate3, /created at Phase 1/);
  assert.doesNotMatch(gate3, /doc_id=<spec doc_id>/);
  // ... it creates the spec doc (slug, doc_type spec, no doc_id) with the requirements-spec tag.
  assert.match(gate3, /handoff_doc_save\(slug="spec-<topic-slug>", title, doc_type="spec", tags=\["spec-loop", "requirements-spec"\]/);
  // Phase 4 updates that doc_id and never reuses the intermediate ones.
  assert.match(phase4, /created at the Phase 3 gate/);
  assert.match(phase4, /Never reuse the `doc_id` of the Phase 1 or\s+Phase 2 intermediate documents/);
});

test('spec-loop command describes the same document model as the skill', () => {
  const cmd = readFileSync(join(HERE, '..', '..', 'commands', 'spec-loop.md'), 'utf8');
  assert.doesNotMatch(cmd, /create at Phase 1/);
  assert.match(cmd, /created once, at the Phase 3 gate/);
  assert.match(cmd, /Never\s+reuse the Phase 1 or Phase 2 intermediate document ids/);
});
