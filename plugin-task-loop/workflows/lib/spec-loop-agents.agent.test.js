// Contract check for the spec-loop agents (REQ-SL-007) and the Phase 0 third-party tool
// check (REQ-SL-011): agents/spec-architect.md, spec-gui-designer.md, spec-usecase-analyst.md
// and agents/_spec-loop-tool-check.md.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENTS = join(HERE, '..', '..', 'agents');

function parse(name) {
  const text = readFileSync(join(AGENTS, `${name}.md`), 'utf8');
  const m = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  assert.ok(m, `${name}: file must start with a --- frontmatter block`);
  const fm = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    assert.ok(kv, `${name}: unparseable frontmatter line: ${line}`);
    fm[kv[1]] = kv[2].trim();
  }
  return { fm, body: m[2] };
}

// name -> [phase marker, diagram types the agent must name, extra needles]
const AGENTS_UNDER_TEST = {
  'spec-architect': ['Phase 1', ['flowchart', 'C4Context'], ['BS-', 'scope']],
  'spec-gui-designer': ['Phase 2', ['stateDiagram-v2', 'HTML'], ['screens', 'output_dir']],
  'spec-usecase-analyst': ['Phase 3', ['sequenceDiagram'], ['BS-', 'actor']],
};

for (const [name, [phase, diagrams, extra]] of Object.entries(AGENTS_UNDER_TEST)) {
  test(`${name}: frontmatter names the agent, model sonnet, and a usable tool list`, () => {
    const { fm } = parse(name);
    assert.equal(fm.name, name);
    assert.equal(fm.model, 'sonnet');
    assert.ok(fm.description.length > 0);
    assert.ok(fm.color.length > 0);
    const tools = fm.tools.split(',').map((t) => t.trim());
    for (const t of ['Read', 'Bash', 'Grep']) assert.ok(tools.includes(t), `tools must include ${t}`);
  });

  test(`${name}: states its phase, role, Input and Output, and the discard notice`, () => {
    const { body } = parse(name);
    for (const needle of [phase, '## Input', '## Output', 'Important', 'context is discarded']) {
      assert.ok(body.includes(needle), `body must mention ${needle}`);
    }
  });

  test(`${name}: names its diagram types / artifacts and domain vocabulary`, () => {
    const { body } = parse(name);
    for (const needle of [...diagrams, ...extra]) assert.ok(body.includes(needle), `body must mention ${needle}`);
  });

  test(`${name}: validates Mermaid with mmdc and forbids touching .handoff/ and state tools`, () => {
    const { body } = parse(name);
    assert.match(body, /mmdc/);
    assert.match(body, /Do not touch `\.handoff\/`/);
    assert.match(body, /Do NOT call any state-modifying handoff tools/);
  });
}

test('only spec-gui-designer holds Write (HTML mocks); the other analysts return text', () => {
  for (const name of Object.keys(AGENTS_UNDER_TEST)) {
    const tools = parse(name).fm.tools.split(',').map((t) => t.trim());
    assert.equal(tools.includes('Write'), name === 'spec-gui-designer', `${name}: Write tool`);
    assert.ok(!tools.includes('Edit'), `${name}: must not hold Edit`);
  }
});

test('_spec-loop-tool-check: classifies mmdc as required and Typst as optional, per tool with install + license', () => {
  const text = readFileSync(join(AGENTS, '_spec-loop-tool-check.md'), 'utf8');
  for (const needle of [
    'Phase 0',
    'typst', 'mmdc', 'chrome-headless-shell',
    'HANDOFF_TYPST_BIN', 'HANDOFF_MMDC_BIN', 'HANDOFF_CHROME_BIN',
    'npm install -g @mermaid-js/mermaid-cli',
    'brew install typst',
    'npx puppeteer browsers install chrome-headless-shell',
    'Apache-2.0', 'MIT', 'BSD-3-Clause',
    'THIRD_PARTY_LICENSES',
  ]) assert.ok(text.includes(needle), `tool check must mention ${needle}`);
  assert.match(text, /mmdc[^\n]*\*\*required\*\*/i);
  assert.match(text, /Typst[^\n]*\*\*optional\*\*/i);
});
