import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  ROLES,
  effortForRole,
  handoffToolsForRole,
  buildHandoffContextSection,
  buildInjectedContextSection,
} from './context-injection.js';

// ============================================================
// effortForRole — reasoning effort follows the pipeline depth
// ============================================================
test('express downgrades the developer to medium effort', () => {
  assert.equal(effortForRole('express', 'developer'), 'medium');
});

test('standard and full keep the developer at high effort', () => {
  assert.equal(effortForRole('standard', 'developer'), 'high');
  assert.equal(effortForRole('full', 'developer'), 'high');
});

test('the tester always reasons at high effort — it is the adversarial layer', () => {
  assert.equal(effortForRole('standard', 'tester'), 'high');
  assert.equal(effortForRole('full', 'tester'), 'high');
});

test('the reviewer reasons at high effort (it only runs under full)', () => {
  assert.equal(effortForRole('full', 'reviewer'), 'high');
});

test('effortForRole normalizes the profile the same way resolveProfile does', () => {
  assert.equal(effortForRole('EXPRESS', 'developer'), 'medium');
  assert.equal(effortForRole(undefined, 'developer'), 'high', 'default profile is standard');
});

test('an unknown profile throws rather than silently picking an effort', () => {
  assert.throws(() => effortForRole('turbo', 'developer'), /unknown profile/);
});

test('an unknown role throws — a typo must not silently inherit session effort', () => {
  assert.throws(() => effortForRole('standard', 'auditor'), /unknown role/);
  assert.throws(() => effortForRole('standard', undefined), /unknown role/);
});

test('ROLES lists exactly the four agent roles', () => {
  assert.deepEqual([...ROLES].sort(), ['developer', 'integration-tester', 'reviewer', 'tester']);
});

test('every role has an effort under every profile — none can return undefined', () => {
  for (const p of ['express', 'standard', 'full']) {
    for (const role of ROLES) {
      assert.ok(effortForRole(p, role), `${p}/${role} has no effort`);
    }
  }
});

test('the integration tester always reasons at high effort — it is an adversarial layer', () => {
  assert.equal(effortForRole('standard', 'integration-tester'), 'high');
  assert.equal(effortForRole('full', 'integration-tester'), 'high');
});

test('the integration tester keeps get_task and memory_query, and is not handed list_tasks', () => {
  const tools = handoffToolsForRole('integration-tester', 'standard');
  assert.deepEqual(tools, ['handoff_get_task', 'handoff_memory_query']);
});

test('only the reviewer may write handoff state; the integration tester may not', () => {
  assert.throws(
    () => buildHandoffContextSection('integration-tester', 'full', { allowWrites: true }),
    /only the reviewer may write handoff state/,
  );
});

test('the integration tester is told not to call load_context, like every other role', () => {
  const s = buildHandoffContextSection('integration-tester', 'standard');
  assert.match(s, /Do not call `handoff_load_context`/);
  assert.match(s, /Do NOT call any state-modifying handoff tools/);
});

// ============================================================
// handoffToolsForRole — who still fetches what for themselves
// ============================================================
test('the developer keeps get_task and memory_query, and never load_context', () => {
  const tools = handoffToolsForRole('developer', 'standard');
  assert.ok(tools.includes('handoff_get_task'), 'notes/labels/links/dependencies are not injected');
  assert.ok(tools.includes('handoff_memory_query'), 'the memory to fetch depends on the files touched');
  assert.ok(!tools.includes('handoff_load_context'), 'load_context is fetched once by the manager');
  assert.ok(!tools.includes('handoff_list_tasks'));
});

test('the tester keeps memory_query but not list_tasks', () => {
  const tools = handoffToolsForRole('tester', 'standard');
  assert.ok(tools.includes('handoff_memory_query'), 'prior-bug lookup is the adversarial check itself');
  assert.ok(!tools.includes('handoff_load_context'));
  assert.ok(!tools.includes('handoff_list_tasks'));
});

test('the reviewer keeps list_tasks — cross-task duplicate detection is its own value', () => {
  const tools = handoffToolsForRole('reviewer', 'full');
  assert.ok(tools.includes('handoff_list_tasks'));
  assert.ok(tools.includes('handoff_memory_query'));
  assert.ok(!tools.includes('handoff_load_context'));
});

test('no role calls load_context — it is fetched once and injected', () => {
  for (const role of ROLES) {
    const profile = role === 'reviewer' ? 'full' : 'standard';
    assert.ok(
      !handoffToolsForRole(role, profile).includes('handoff_load_context'),
      `${role} must not call handoff_load_context`,
    );
  }
});

test('handoffToolsForRole returns a fresh array; mutating it cannot poison the table', () => {
  const a = handoffToolsForRole('developer', 'standard');
  a.push('handoff_save_context');
  assert.ok(!handoffToolsForRole('developer', 'standard').includes('handoff_save_context'));
});

test('an unknown role throws', () => {
  assert.throws(() => handoffToolsForRole('auditor', 'standard'), /unknown role/);
});

// ============================================================
// buildHandoffContextSection — role-specific, not one-size-fits-all
// ============================================================
/** The tool-listing bullets, i.e. the calls the section tells the role to make. */
const listedTools = (section) =>
  section
    .split('\n')
    .filter((l) => l.startsWith('- `handoff_'))
    .map((l) => l.match(/^- `([^`]+)`/)[1]);

test('the section lists only the tools that role should call', () => {
  const dev = buildHandoffContextSection('developer', 'standard');
  assert.deepEqual(listedTools(dev), ['handoff_get_task', 'handoff_memory_query']);
});

test('the section explicitly forbids load_context rather than staying silent', () => {
  const dev = buildHandoffContextSection('developer', 'standard');
  assert.ok(!listedTools(dev).includes('handoff_load_context'), 'not offered as a call to make');
  assert.match(dev, /Do not call `handoff_load_context`/, 'but named, so the agent does not reach for it');
});

test('the reviewer section lists list_tasks; the developer section does not', () => {
  assert.ok(listedTools(buildHandoffContextSection('reviewer', 'full')).includes('handoff_list_tasks'));
  assert.ok(!listedTools(buildHandoffContextSection('developer', 'standard')).includes('handoff_list_tasks'));
});

test('every section forbids state-modifying handoff tools by default', () => {
  for (const role of ROLES) {
    const profile = role === 'reviewer' ? 'full' : 'standard';
    assert.match(
      buildHandoffContextSection(role, profile),
      /Do NOT call any state-modifying handoff tools/,
      `${role} must be told not to write handoff state`,
    );
  }
});

// The escalation round REQUIRES the reviewer to call handoff_save_context. A
// blanket prohibition in the same prompt would contradict that mandate and leave
// the agent to guess which instruction governs.
test('allowWrites drops the blanket prohibition instead of contradicting escalation', () => {
  const section = buildHandoffContextSection('reviewer', 'full', { allowWrites: true });
  assert.doesNotMatch(section, /Do NOT call any state-modifying handoff tools/);
  assert.match(section, /Writes are permitted this round/);
});

test('allowWrites still fences off task and session state', () => {
  const section = buildHandoffContextSection('reviewer', 'full', { allowWrites: true });
  assert.match(section, /handoff_update_task/, 'still named as off-limits');
  assert.match(section, /manager's job/);
});

test('allowWrites is reviewer-only — a developer that could write handoff state is a bug', () => {
  assert.throws(
    () => buildHandoffContextSection('developer', 'standard', { allowWrites: true }),
    /only the reviewer may write handoff state/,
  );
  assert.throws(() => buildHandoffContextSection('tester', 'standard', { allowWrites: true }), /only the reviewer/);
});

test('omitting opts keeps the prohibition (writes are opt-in, never default)', () => {
  assert.match(
    buildHandoffContextSection('reviewer', 'full', {}),
    /Do NOT call any state-modifying handoff tools/,
  );
  assert.match(
    buildHandoffContextSection('reviewer', 'full', { allowWrites: false }),
    /Do NOT call any state-modifying handoff tools/,
  );
});

test('every section states that session context is already injected', () => {
  for (const role of ROLES) {
    const profile = role === 'reviewer' ? 'full' : 'standard';
    const section = buildHandoffContextSection(role, profile);
    assert.match(section, /already been fetched .* injected/i);
  }
});

test('the express developer is told not to spend turns on optional lookups', () => {
  const express = buildHandoffContextSection('developer', 'express');
  const standard = buildHandoffContextSection('developer', 'standard');
  assert.match(express, /express/i);
  assert.match(express, /skip/i);
  assert.doesNotMatch(standard, /skip/i);
});

test('the express developer still keeps its required tools listed', () => {
  const express = buildHandoffContextSection('developer', 'express');
  assert.match(express, /handoff_get_task/);
  assert.match(express, /handoff_memory_query/);
});

// ============================================================
// buildInjectedContextSection — the fetch-once payload
// ============================================================
test('prev_session_summary is rendered (it used to be a dead arg)', () => {
  const s = buildInjectedContextSection({
    branch: 'feat/x',
    prev_session_summary: 'Finished t75 and t76.',
  });
  assert.match(s, /Finished t75 and t76\./);
});

test('design_decisions is rendered', () => {
  const s = buildInjectedContextSection({ design_decisions: 'Use fail-closed verdicts.' });
  assert.match(s, /Use fail-closed verdicts\./);
});

test('handoff_context.decisions are rendered as bullets with their reason', () => {
  const s = buildInjectedContextSection({
    handoff_context: {
      decisions: [{ decision: 'Default profile is standard', reason: 'User confirmed', confidence: 'confirmed' }],
    },
  });
  assert.match(s, /Default profile is standard/);
  assert.match(s, /User confirmed/);
  assert.match(s, /confirmed/);
});

test('handoff_context.handoff_notes are rendered with their category', () => {
  const s = buildInjectedContextSection({
    handoff_context: {
      handoff_notes: [{ category: 'caution', note: 'Sync generated blocks first' }],
    },
  });
  assert.match(s, /caution/);
  assert.match(s, /Sync generated blocks first/);
});

test('handoff_context.next_actions are rendered', () => {
  const s = buildInjectedContextSection({ handoff_context: { next_actions: ['Implement t77'] } });
  assert.match(s, /Implement t77/);
});

test('handoff_context.memories are rendered so no agent needs a blanket memory sweep', () => {
  const s = buildInjectedContextSection({
    handoff_context: { memories: [{ title: 'tmp naming', content: 'Use YYMMNN' }] },
  });
  assert.match(s, /tmp naming/);
  assert.match(s, /Use YYMMNN/);
});

test('a plain string handoff_context is passed through verbatim', () => {
  const s = buildInjectedContextSection({ handoff_context: 'raw blob from the manager' });
  assert.match(s, /raw blob from the manager/);
});

test('an empty session context yields an explicit "None" rather than a dangling heading', () => {
  const s = buildInjectedContextSection({ branch: 'feat/x' });
  assert.match(s, /None/);
  assert.doesNotMatch(s, /undefined/);
});

test('a missing session context does not throw', () => {
  assert.doesNotThrow(() => buildInjectedContextSection(undefined));
  assert.doesNotThrow(() => buildInjectedContextSection(null));
});

test('branch is not duplicated here — the prompt header already states it', () => {
  const s = buildInjectedContextSection({ branch: 'feat/only-branch' });
  assert.doesNotMatch(s, /feat\/only-branch/);
});

test('every rendered subsection carries a heading so the agent can navigate it', () => {
  const s = buildInjectedContextSection({
    prev_session_summary: 'S',
    design_decisions: 'D',
    handoff_context: {
      decisions: [{ decision: 'X' }],
      handoff_notes: [{ category: 'suggestion', note: 'Y' }],
      next_actions: ['Z'],
      memories: [{ title: 'M', content: 'C' }],
    },
  });
  for (const heading of [
    /Previous session summary/i,
    /Design decisions/i,
    /Inherited decisions/i,
    /Handoff notes/i,
    /Next actions/i,
    /Project memory/i,
  ]) {
    assert.match(s, heading);
  }
});

test('no information is lost: a decision without a reason still renders its text', () => {
  const s = buildInjectedContextSection({ handoff_context: { decisions: [{ decision: 'Bare decision' }] } });
  assert.match(s, /Bare decision/);
  assert.doesNotMatch(s, /undefined/);
});

// `handoff_load_context` nests decisions / handoff_notes under `previous_session`.
// A manager that passes the tool's response straight through — the obvious thing
// to do — would otherwise have them silently dropped: no error, no warning, the
// agent just never sees them. That is the exact defect class this task removes.
test('the raw handoff_load_context response shape is accepted verbatim', () => {
  const raw = {
    project: 'handoff-mcp',
    task_tree: [{ id: 't77', status: 'todo' }],
    session_guidance: { action: 'create_session' },
    next_actions: ['TOP_ACTION'],
    previous_session: {
      summary: 'NESTED_SUMMARY',
      decisions: [{ decision: 'NESTED_DECISION', reason: 'r' }],
      handoff_notes: [{ category: 'caution', note: 'NESTED_NOTE' }],
    },
  };
  const s = buildInjectedContextSection({ branch: 'b', handoff_context: raw });
  assert.match(s, /NESTED_DECISION/, 'decisions under previous_session must not be dropped');
  assert.match(s, /NESTED_NOTE/, 'handoff_notes under previous_session must not be dropped');
  assert.match(s, /TOP_ACTION/);
});

test('the previous session summary is picked up from previous_session.summary', () => {
  const s = buildInjectedContextSection({
    handoff_context: { previous_session: { summary: 'NESTED_SUMMARY' } },
  });
  assert.match(s, /NESTED_SUMMARY/);
});

test('an explicit prev_session_summary wins over the nested one', () => {
  const s = buildInjectedContextSection({
    prev_session_summary: 'EXPLICIT',
    handoff_context: { previous_session: { summary: 'NESTED' } },
  });
  assert.match(s, /EXPLICIT/);
  assert.doesNotMatch(s, /NESTED/, 'the caller-supplied summary must not be duplicated by the nested one');
});

test('a top-level field wins over the same field nested under previous_session', () => {
  const s = buildInjectedContextSection({
    handoff_context: {
      decisions: [{ decision: 'TOP_DEC' }],
      previous_session: { decisions: [{ decision: 'NESTED_DEC' }] },
    },
  });
  assert.match(s, /TOP_DEC/);
  assert.doesNotMatch(s, /NESTED_DEC/);
});

test('irrelevant load_context keys are not spilled into the prompt', () => {
  const s = buildInjectedContextSection({
    handoff_context: { session_guidance: { action: 'create_session' }, task_summary: { total: 39 } },
  });
  assert.doesNotMatch(s, /create_session/);
  assert.doesNotMatch(s, /task_summary/);
});

test('a malformed handoff_context entry is skipped, not rendered as undefined', () => {
  const s = buildInjectedContextSection({
    handoff_context: { decisions: [null, { decision: 'Kept' }], handoff_notes: [null], memories: [null] },
  });
  assert.match(s, /Kept/);
  assert.doesNotMatch(s, /undefined/);
  assert.doesNotMatch(s, /null/);
});
