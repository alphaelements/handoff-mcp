// ============================================================
// elite-report — read the elite-developer's single report for session-execute
// ============================================================
// SINGLE SOURCE OF TRUTH. See lib/verdict-logic.js for why this file is
// mirrored rather than imported: the Workflow runtime rejects import()/require,
// and session-execute.js has a top-level `return` so Node cannot import it.
//
// Edit THIS file, then run `scripts/sync-workflow-inline.sh` to sync.
//
// Everything between the INLINE markers must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no module-level mutable state.
//
// One exception: the inline region calls `escapeRegExp` from the `verdict-logic`
// module. Both regions are mirrored into the same script and `escapeRegExp` is a
// function declaration, so it is hoisted and in scope wherever this block lands.
// The import below exists only so this file is testable on its own, and it lives
// OUTSIDE the markers so it is never copied into the workflow.

import { escapeRegExp } from './verdict-logic.js';

// --- BEGIN INLINE: elite-report ---

/**
 * Under the `elite` profile one agent implements, tests, and reviews, and hands
 * back ONE markdown report (agents/elite-developer.md, "Return format"). The
 * stages after it were written for a pipeline with three reports: a developer
 * report per task for the closer, a tester verdict, and a reviewer verdict.
 *
 * This module maps the one onto the three, so the closer, the spec-reconciler,
 * and the follow-up filing keep reading the shapes they already read:
 *
 *   `## dev result: <task_id>` block        -> that task's entry in `dev_reports`
 *   `#### full_suite`                       -> `integration_report`
 *   `#### review` / `#### adversarial_test`
 *     / `#### fix_loop` / `### pendingFollowups` -> `review_report`
 *
 * Everything here fails closed. Nobody runs after the elite agent to notice a
 * section it left out, so a section that is missing or cannot be read is never
 * taken as a pass: it becomes a finding, and the finding says what is missing.
 */

/** `source` of a follow-up that came out of an elite report, and the marker on a derived report. */
const ELITE_REPORT_SOURCE = 'elite-developer';

const ELITE_FINDING_SEVERITIES = Object.freeze(['BLOCKER', 'MAJOR', 'MINOR', 'NIT']);

/**
 * Severity of an open finding that names none. MAJOR, as everywhere else in this
 * workflow (`f.severity || 'MAJOR'`): an open item of unknown weight must not be
 * read as a nit.
 */
const ELITE_DEFAULT_SEVERITY = 'MAJOR';

/**
 * The seven stages the elite-developer runs for each task, in order: the
 * `#### <stage>` sections of its report (agents/elite-developer.md).
 */
const ELITE_STAGES = Object.freeze([
  'spec_check',
  'implementation',
  'adversarial_test',
  'full_suite',
  'review',
  'fix_loop',
  'spec_reconcile',
]);

/**
 * The `- status:` values each stage may report. `fix_loop` has no status line:
 * it reports counts, which eliteStageStatus() reads instead.
 */
const ELITE_STAGE_STATUSES = Object.freeze({
  spec_check: Object.freeze(['pass', 'warn']),
  implementation: Object.freeze(['pass', 'fail']),
  adversarial_test: Object.freeze(['pass', 'findings']),
  full_suite: Object.freeze(['pass', 'fail']),
  review: Object.freeze(['approve', 'request_changes']),
  spec_reconcile: Object.freeze(['pass', 'updated']),
});

/** How a stage with no readable result is shown in the progress log. */
const ELITE_STAGE_NOT_REPORTED = 'not reported';

/** Index of the task a `## dev result:` heading names, or -1. Whole-id match: t1 is not t12. */
function eliteBlockOwner(headingRest, taskIds) {
  let best = -1;
  for (let i = 0; i < taskIds.length; i++) {
    const named = new RegExp(`^[\`*]*${escapeRegExp(taskIds[i])}(?![\\w.+-])`).test(headingRest);
    if (named && (best < 0 || String(taskIds[i]).length > String(taskIds[best]).length)) best = i;
  }
  return best;
}

/**
 * Cut the report into one block per task, at its `## dev result: <task_id>`
 * headings. Returns an array aligned with `taskIds`; a task with no block of its
 * own is `null`, which the caller must treat as "not reported", never as done.
 *
 * A second block for the same task replaces the first: the report is written in
 * the order the work happened, so the later block is the agent's final word.
 */
function splitEliteReport(report, taskIds) {
  const ids = Array.isArray(taskIds) ? taskIds : [];
  const slots = ids.map(() => null);
  if (typeof report !== 'string' || report.trim() === '') return slots;

  const heading = /^##[ \t]+[Dd]ev[ \t]+[Rr]esult[ \t]*:[ \t]*(.*)$/gm;
  const starts = [];
  let m;
  while ((m = heading.exec(report)) !== null) starts.push({ index: m.index, rest: m[1] });

  for (let i = 0; i < starts.length; i++) {
    const owner = eliteBlockOwner(starts[i].rest, ids);
    if (owner < 0) continue;
    const end = i + 1 < starts.length ? starts[i + 1].index : report.length;
    slots[owner] = report.slice(starts[i].index, end).trim();
  }
  return slots;
}

/** Body of a `#### <stage>` subsection, or null when the block has none. */
function eliteStageSection(block, stage) {
  const m = block.match(
    new RegExp(`^####[ \\t]+${escapeRegExp(stage)}(?![\\w])[^\\n]*\\n?([\\s\\S]*?)(?=^#{1,4}[ \\t]|(?![\\s\\S]))`, 'm'),
  );
  return m ? m[1] : null;
}

/** Body of the `### pendingFollowups` section, or null when the block has none. */
function elitePendingSection(block) {
  const m = block.match(
    /^###[ \t]+pending[ \t_-]*follow[ \t_-]*ups?(?![\w])[^\n]*\n?([\s\S]*?)(?=^#{1,3}[ \t]|(?![\s\S]))/im,
  );
  return m ? m[1] : null;
}

/** Raw value of a `- <name>: <value>` line, or null. */
function eliteFieldValue(section, name) {
  if (typeof section !== 'string') return null;
  const m = section.match(new RegExp(`^[ \\t]*[-*][ \\t]+(?:\\*\\*)?${name}(?:\\*\\*)?[ \\t]*:[ \\t]*(.*)$`, 'm'));
  return m ? m[1].trim() : null;
}

/**
 * Read one enum token out of a raw field value. A trailing remark is fine
 * (`pass (132 tests)`), but the token must be whole and must not be followed by
 * `|` or `/`: the report template itself says `pass|fail`, and a template line
 * echoed back is not a result.
 */
function eliteEnumValue(raw, allowed) {
  if (typeof raw !== 'string') return null;
  const m = raw.replace(/^[^A-Za-z]+/, '').match(/^([A-Za-z_]+)(?![A-Za-z_])(?![ \t`*]*[|/])/);
  if (!m) return null;
  const value = m[1].toLowerCase();
  return allowed.includes(value) ? value : null;
}

function eliteIntValue(raw) {
  if (typeof raw !== 'string') return null;
  const n = Number.parseInt(raw, 10);
  return Number.isInteger(n) ? n : null;
}

/** Is this the agent saying "nothing here" (`None`, `N/A`, `(or "None")`, `なし`)? */
function isEliteNone(text) {
  const t = String(text).trim().replace(/^[\s("'`*_[]+/, '');
  return /^(?:or[ \t]+["'`*_]*)?(?:none|n\/a)(?![\w])/i.test(t) || /^(?:該当)?(?:なし|無し)/.test(t);
}

/** Does the text say anything at all (a lone `]` or `-` does not)? */
function eliteHasContent(text) {
  return /[\p{L}\p{N}]/u.test(text);
}

/**
 * Entries of a `- <name>: [...]` list field, written inline or as nested bullets.
 * An inline list stays ONE entry: its items are free prose with commas of their
 * own, so splitting on commas would cut findings in half.
 */
function eliteListField(section, name) {
  if (typeof section !== 'string') return [];
  const lines = section.split('\n');
  const key = new RegExp(`^([ \\t]*)[-*][ \\t]+(?:\\*\\*)?${name}(?:\\*\\*)?[ \\t]*:[ \\t]*(.*)$`);
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(key);
    if (!m) continue;
    const entries = [];
    const inline = m[2].trim().replace(/^\[/, '').replace(/\]$/, '').trim();
    if (inline !== '') entries.push(inline);
    for (let j = i + 1; j < lines.length; j++) {
      if (lines[j].trim() === '') continue;
      if (lines[j].match(/^[ \t]*/)[0].length <= m[1].length) break;
      entries.push(lines[j].trim().replace(/^[-*][ \t]+/, ''));
    }
    return entries.filter((e) => eliteHasContent(e) && !isEliteNone(e));
  }
  return [];
}

/**
 * Entries of the `### pendingFollowups` section: one per top-level line, with
 * more-indented lines folded into the entry above them. Every line that is not
 * a way of saying "none" counts, bullet or not, in the documented slash format
 * or not: an open item the agent phrased loosely is still open.
 */
function elitePendingEntries(sectionBody) {
  const entries = [];
  let base = null;
  for (const raw of sectionBody.split('\n')) {
    if (raw.trim() === '') continue;
    const indent = raw.match(/^[ \t]*/)[0].length;
    const text = raw.trim().replace(/^[-*][ \t]+/, '');
    if (base !== null && indent > base && entries.length > 0) {
      entries[entries.length - 1] += ` ${text}`;
      continue;
    }
    if (base === null) base = indent;
    entries.push(text);
  }
  return entries.filter((e) => eliteHasContent(e) && !isEliteNone(e));
}

/**
 * One `### pendingFollowups` entry as a finding. The documented line is
 * `<task_id> / <severity> / <file:line> / <problem>`. The owner defaults to the
 * block the entry was found in; an entry may hand itself to another task of the
 * session or to `*`, and an owner this session does not know stays in the text.
 */
function eliteFollowupFinding(entry, blockTaskId, taskIds) {
  const parts = entry.split(/[ \t]+\/[ \t]+/);
  const stated = parts.length >= 4 ? parts[1].replace(/[^A-Za-z]/g, '').toUpperCase() : '';
  if (!ELITE_FINDING_SEVERITIES.includes(stated)) {
    const sev = entry.match(/\b(BLOCKER|MAJOR|MINOR|NIT)\b/);
    return { task_id: blockTaskId, severity: sev ? sev[1] : ELITE_DEFAULT_SEVERITY, location: null, problem: entry };
  }

  const owner = parts[0].replace(/[`<>]/g, '').trim();
  const where = parts[2].replace(/`/g, '').trim();
  const problem = parts.slice(3).join(' / ').trim();
  const known = owner === '*' || taskIds.includes(owner);
  return {
    task_id: known ? owner : blockTaskId,
    severity: stated,
    location: /^(?:-|—|n\/a|none)?$/i.test(where) ? null : where,
    problem: known ? problem : `${parts[0].trim()}: ${problem}`,
  };
}

/** Everything the derivation reads from one task's block. Absent parts are null, never guessed. */
function parseEliteTaskBlock(block, taskId, taskIds) {
  const head = block.split(/^###/m)[0];
  const statusLine = head.match(/^[ \t]*(?:\*\*)?status(?:\*\*)?[ \t]*:[ \t]*(.*)$/m);
  const fullSuite = eliteStageSection(block, 'full_suite');
  const review = eliteStageSection(block, 'review');
  const adversarial = eliteStageSection(block, 'adversarial_test');
  const fixLoop = eliteStageSection(block, 'fix_loop');
  const pending = elitePendingSection(block);

  return {
    status: statusLine ? eliteEnumValue(statusLine[1], ['done', 'needs_more_work', 'blocked']) : null,
    full_suite: {
      text: fullSuite,
      status: eliteEnumValue(eliteFieldValue(fullSuite, 'status'), ELITE_STAGE_STATUSES.full_suite),
      passed: eliteIntValue(eliteFieldValue(fullSuite, 'passed')),
      failed: eliteIntValue(eliteFieldValue(fullSuite, 'failed')),
      skipped: eliteIntValue(eliteFieldValue(fullSuite, 'skipped')),
    },
    review: {
      text: review,
      status: eliteEnumValue(eliteFieldValue(review, 'status'), ELITE_STAGE_STATUSES.review),
    },
    adversarial_test: {
      text: adversarial,
      status: eliteEnumValue(eliteFieldValue(adversarial, 'status'), ELITE_STAGE_STATUSES.adversarial_test),
    },
    fix_loop: {
      text: fixLoop,
      iterations: eliteIntValue(eliteFieldValue(fixLoop, 'iterations')),
      unresolved: eliteListField(fixLoop, 'unresolved'),
    },
    pending_text: pending,
    // null = the section is absent, which is not the same as "nothing pending".
    followups:
      pending === null ? null : elitePendingEntries(pending).map((e) => eliteFollowupFinding(e, taskId, taskIds)),
  };
}

/**
 * Derive the tester-shaped and reviewer-shaped results from the per-task blocks
 * (`blocks[i]` belongs to `taskIds[i]`; see splitEliteReport).
 *
 * Both results have the fields the multi-agent pipeline's results have
 * (`verdict`, `findings[]`, `report`), so `normalizeIntegrationVerdict`,
 * `normalizeReviewVerdict`, and `extractUnresolvedFindings` read them as they
 * are. Two more fields say where they came from: `derived_from`, and
 * `missing_sections` (`"<task_id>: <section>"` for every part that was absent or
 * unreadable).
 *
 * integration_report — the whole-tree gates.
 *   PASS only if every task's `full_suite` reads `status: pass` and counts no
 *   failure. `full_suite` reports the last full run, so it is already the final
 *   state.
 *
 * review_report — is anything still open?
 *   Deliberately NOT a copy of `review.status`. A stage reports what it found,
 *   and a review whose findings the fix loop then resolved still reads
 *   `request_changes`; copying it would hold every such task in `review` forever.
 *   As with the reviewer's own verdict, APPROVE means "nothing is left open":
 *   the review ran, and neither `### pendingFollowups` nor `fix_loop.unresolved`
 *   lists anything. Three more things are not an approval: a block whose
 *   `**status**` is not `done` while it lists nothing open, stage findings that
 *   no fix-loop iteration accounts for, and a missing section.
 */
function deriveEliteVerdicts(blocks, taskIds) {
  const ids = Array.isArray(taskIds) ? taskIds : [];
  const integration = { findings: [], missing: [], text: [] };
  const review = { findings: [], missing: [], text: [] };
  const finding = (task_id, severity, problem) => ({ task_id, severity, location: null, problem });
  const count = (n) => (n === null ? '?' : n);

  ids.forEach((id, i) => {
    const block = Array.isArray(blocks) ? blocks[i] : null;
    if (typeof block !== 'string' || block.trim() === '') {
      const problem =
        `The elite-developer returned no \`## dev result: ${id}\` block. ` +
        `Nothing was reported for this task, so nothing about it is verified.`;
      for (const side of [integration, review]) {
        side.missing.push(`${id}: dev result`);
        side.findings.push(finding(id, 'BLOCKER', problem));
        side.text.push(`### ${id}\n(no \`## dev result\` block)`);
      }
      return;
    }

    const p = parseEliteTaskBlock(block, id, ids);

    // --- full_suite -> integration_report ---
    const suite = p.full_suite;
    integration.text.push(`### ${id}\n#### full_suite\n${suite.text === null ? '(section missing)' : suite.text.trim()}`);
    let suiteFailed = false;
    if (suite.status === null) {
      suiteFailed = true;
      integration.missing.push(`${id}: full_suite`);
      integration.findings.push(
        finding(
          id,
          'BLOCKER',
          `The report for ${id} has no readable \`#### full_suite\` status (expected pass or fail). ` +
            `The whole-project suite, E2E, and gates are unverified for this task.`,
        ),
      );
    } else if (suite.status !== 'pass' || (suite.failed !== null && suite.failed > 0)) {
      suiteFailed = true;
      integration.findings.push(
        finding(
          id,
          'BLOCKER',
          `full_suite did not pass for ${id} (status: ${suite.status}, passed: ${count(suite.passed)}, ` +
            `failed: ${count(suite.failed)}, skipped: ${count(suite.skipped)}).`,
        ),
      );
    }

    // --- review / adversarial_test / fix_loop / pendingFollowups -> review_report ---
    review.text.push(
      [
        `### ${id}`,
        ...['adversarial_test', 'review', 'fix_loop'].map(
          (s) => `#### ${s}\n${p[s].text === null ? '(section missing)' : p[s].text.trim()}`,
        ),
        `#### pendingFollowups\n${p.pending_text === null ? '(section missing)' : p.pending_text.trim()}`,
      ].join('\n'),
    );

    if (p.review.status === null) {
      review.missing.push(`${id}: review`);
      review.findings.push(
        finding(
          id,
          'BLOCKER',
          `The report for ${id} has no readable \`#### review\` status (expected approve or request_changes). ` +
            `No review of this task is on record.`,
        ),
      );
    }
    if (p.followups === null) {
      review.missing.push(`${id}: pendingFollowups`);
      review.findings.push(
        finding(
          id,
          ELITE_DEFAULT_SEVERITY,
          `The report for ${id} has no \`### pendingFollowups\` section, so open findings cannot be ruled out.`,
        ),
      );
    }

    // `### pendingFollowups` is where open work is handed on, one line per item.
    // `fix_loop.unresolved` is read only when that section lists nothing, so a
    // report that forgot to repeat its unresolved items still shows them.
    const listed = p.followups || [];
    const open =
      listed.length > 0
        ? listed
        : p.fix_loop.unresolved.map((entry) => {
            const sev = entry.match(/\b(BLOCKER|MAJOR|MINOR|NIT)\b/);
            return finding(id, sev ? sev[1] : ELITE_DEFAULT_SEVERITY, `fix_loop.unresolved: ${entry}`);
          });
    review.findings.push(...open);

    if (open.length === 0) {
      const stagesFoundSomething = p.review.status === 'request_changes' || p.adversarial_test.status === 'findings';
      const fixLoopRan = p.fix_loop.iterations !== null && p.fix_loop.iterations >= 1;
      if (stagesFoundSomething && !fixLoopRan) {
        review.findings.push(
          finding(
            id,
            ELITE_DEFAULT_SEVERITY,
            `adversarial_test / review reported findings for ${id}, but fix_loop shows no iteration and nothing ` +
              `is listed as unresolved: those findings are unaccounted for.`,
          ),
        );
      }
      // A failed suite already explains a block that does not claim `done`.
      if (!suiteFailed && (p.status === 'needs_more_work' || p.status === 'blocked')) {
        review.findings.push(
          finding(
            id,
            ELITE_DEFAULT_SEVERITY,
            `The report for ${id} says **status**: ${p.status} but lists no unresolved finding.`,
          ),
        );
      }
    }
  });

  return {
    integration_report: {
      verdict: integration.findings.length === 0 ? 'PASS' : 'FAIL',
      findings: integration.findings,
      report: integration.text.join('\n\n'),
      derived_from: ELITE_REPORT_SOURCE,
      missing_sections: integration.missing,
    },
    review_report: {
      verdict: review.findings.length === 0 ? 'APPROVE' : 'REQUEST_CHANGES',
      findings: review.findings,
      report: review.text.join('\n\n'),
      derived_from: ELITE_REPORT_SOURCE,
      missing_sections: review.missing,
    },
  };
}

/**
 * Rework notes for the next elite launch: per task, what the previous launch
 * left open. A finding owned by `*` (or by nothing this session knows) belongs
 * to the seam between tasks, so every task carries it.
 *
 * Returns Map taskId -> note (only tasks with something open).
 */
function extractEliteReworkNotes(integrationReport, reviewReport, taskIds, round) {
  const notes = new Map();
  if (!Array.isArray(taskIds)) return notes;

  const findings = [];
  for (const r of [integrationReport, reviewReport]) {
    if (r && typeof r === 'object' && Array.isArray(r.findings)) {
      findings.push(...r.findings.filter((f) => f && typeof f === 'object'));
    }
  }
  if (findings.length === 0) return notes;

  const known = new Set(taskIds);
  const shared = findings.filter((f) => !known.has(f.task_id));
  const render = (list) =>
    list
      .map((f) => `- [${f.severity || ELITE_DEFAULT_SEVERITY}] ${f.location ? `${f.location} — ` : ''}${f.problem || ''}`.trim())
      .join('\n');

  for (const taskId of taskIds) {
    const mine = findings.filter((f) => f.task_id === taskId).concat(shared);
    if (mine.length === 0) continue;
    notes.set(taskId, `[Unresolved after elite round ${round}]\n${render(mine)}`);
  }
  return notes;
}

// ------------------------------------------------------------
// Observability: what the one agent says about its own stages
// ------------------------------------------------------------
// The workflow launches the elite-developer once per round and cannot see a
// stage begin or end. What it can show is read from the report afterwards. As
// with the verdicts above, nothing is guessed: a stage the report does not
// answer for is null, and the caller shows it as not reported.

/**
 * The result one stage of one task block reports, or null when the block has no
 * readable answer for it. `fix_loop` reports how far it went, since it has no
 * status of its own.
 */
function eliteStageStatus(block, stage) {
  if (typeof block !== 'string') return null;
  const section = eliteStageSection(block, stage);
  if (section === null) return null;
  if (stage === 'fix_loop') {
    const iterations = eliteIntValue(eliteFieldValue(section, 'iterations'));
    if (iterations === null || iterations < 0) return null;
    return `${iterations} iteration(s), ${eliteListField(section, 'unresolved').length} unresolved`;
  }
  return eliteEnumValue(eliteFieldValue(section, 'status'), ELITE_STAGE_STATUSES[stage] || []);
}

/**
 * `{ task_id, stage, status }` for every task and every stage, in task order,
 * then stage order (`blocks[i]` belongs to `taskIds[i]`; see splitEliteReport).
 * `status` is null for a stage that was not reported, and a task without a block
 * still has its seven entries.
 */
function eliteStageProgress(blocks, taskIds) {
  const ids = Array.isArray(taskIds) ? taskIds : [];
  const progress = [];
  ids.forEach((id, i) => {
    const block = Array.isArray(blocks) ? blocks[i] : null;
    for (const stage of ELITE_STAGES) progress.push({ task_id: id, stage, status: eliteStageStatus(block, stage) });
  });
  return progress;
}

/**
 * The `- elapsed_ms:` a stage section reports, as a whole number of
 * milliseconds, or null. Only a plain non-negative number counts, with `ms` as
 * the one unit allowed after it and no other word following: `90s` or `1.5 min`
 * read as milliseconds would be wrong by orders of magnitude, so they are not
 * read at all. A remark in brackets after the number is fine.
 */
function eliteStageElapsedMs(block, stage) {
  if (typeof block !== 'string') return null;
  const raw = eliteFieldValue(eliteStageSection(block, stage), 'elapsed_ms');
  if (raw === null) return null;
  const m = raw.match(/^(\d+(?:\.\d+)?)(?:[ \t]*ms)?(?![ \t]*[\w.])/i);
  return m ? Math.round(Number(m[1])) : null;
}

/**
 * Wall-clock milliseconds per stage, summed over the given task blocks. Every
 * stage of ELITE_STAGES is a key. A stage no block gives a time for is null,
 * which means "not measured" and is not the same as 0.
 *
 * Pass the blocks as each launch returned them, not the per-task list the
 * session keeps: a block carried over from an earlier round would be counted
 * again.
 */
function eliteStageTimings(blocks) {
  const timings = {};
  for (const stage of ELITE_STAGES) {
    let total = null;
    for (const block of Array.isArray(blocks) ? blocks : []) {
      const ms = eliteStageElapsedMs(block, stage);
      if (ms !== null) total = (total === null ? 0 : total) + ms;
    }
    timings[stage] = total;
  }
  return timings;
}

/**
 * The usage metadata an agent returned with its result, or null when it
 * returned none. It is passed on for the manager to display and is never
 * computed with: a non-negative number, or an object, as the agent gave it.
 *
 * Copied by hand into the four workflow scripts this file is not mirrored into
 * (COPY: elite-usage); lib/elite-observability.test.js compares each copy with
 * this function.
 */
function eliteTokenUsage(result) {
  if (result === null || typeof result !== 'object' || Array.isArray(result)) return null;
  const usage = result.token_usage;
  if (typeof usage === 'number') return Number.isFinite(usage) && usage >= 0 ? usage : null;
  return usage !== null && typeof usage === 'object' && !Array.isArray(usage) ? usage : null;
}

/**
 * The usage of a session that launched the agent more than once. A single
 * reported usage is returned as it is. Several are totalled: numbers are added,
 * objects are added field by field, and a field that is not a number is left
 * out, since it has no total. Usages that cannot be added to each other (a
 * number and an object) give null: an unknown total is reported as unknown, not
 * as the last launch's share of it.
 */
function sumEliteTokenUsage(usages) {
  const reported = (Array.isArray(usages) ? usages : []).filter((u) => u !== null && u !== undefined);
  if (reported.length === 0) return null;
  if (reported.length === 1) return reported[0];
  if (reported.every((u) => typeof u === 'number')) return reported.reduce((sum, u) => sum + u, 0);
  if (!reported.every((u) => typeof u === 'object')) return null;

  const total = {};
  for (const usage of reported) {
    for (const [field, value] of Object.entries(usage)) {
      if (typeof value === 'number' && Number.isFinite(value)) total[field] = (total[field] || 0) + value;
    }
  }
  return total;
}

// --- END INLINE: elite-report ---

export {
  ELITE_REPORT_SOURCE,
  ELITE_FINDING_SEVERITIES,
  ELITE_DEFAULT_SEVERITY,
  ELITE_STAGES,
  ELITE_STAGE_STATUSES,
  ELITE_STAGE_NOT_REPORTED,
  eliteStageProgress,
  eliteStageTimings,
  eliteTokenUsage,
  sumEliteTokenUsage,
  splitEliteReport,
  parseEliteTaskBlock,
  deriveEliteVerdicts,
  extractEliteReworkNotes,
};
