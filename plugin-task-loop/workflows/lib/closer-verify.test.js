import { test } from 'node:test';
import assert from 'node:assert/strict';
import { compareCloserClaimsWithDisk } from './closer-verify.js';

const obs = (task_id, status, checked_indices) => ({ task_id, status, checked_indices });

test('an honest closer produces no mismatches', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done', criteria_checked: [0, 1] }],
    [obs('t1', 'done', [0, 1])],
  );
  assert.deepEqual(m, []);
});

test('a claimed status that differs from the on-disk status is a status mismatch', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done', criteria_checked: [] }],
    [obs('t1', 'todo', [])],
  );
  assert.equal(m.length, 1);
  assert.equal(m[0].task_id, 't1');
  assert.equal(m[0].kind, 'status');
  assert.match(m[0].message, /status expected done got todo/);
});

test('a claimed criterion that is unchecked on disk is a criterion mismatch naming the index', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'review', criteria_checked: [0, 1, 3] }],
    [obs('t1', 'review', [0])],
  );
  assert.deepEqual(
    m.map((x) => [x.kind, x.index]),
    [['criterion', 1], ['criterion', 3]],
  );
  assert.match(m[0].message, /criterion index 1 claimed checked but unchecked on disk/);
});

test('a lying closer yields both a status and a criterion mismatch for the task', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done', criteria_checked: [0, 1] }],
    [obs('t1', 'todo', [0])],
  );
  assert.deepEqual(m.map((x) => x.kind), ['status', 'criterion']);
});

test('extra criteria checked on disk that the closer did not claim are not a mismatch', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done', criteria_checked: [0] }],
    [obs('t1', 'done', [0, 1, 2])],
  );
  assert.deepEqual(m, []);
});

test("status_set 'unchanged' or absent is not compared against the disk", () => {
  const m = compareCloserClaimsWithDisk(
    [
      { task_id: 't1', status_set: 'unchanged', criteria_checked: [] },
      { task_id: 't2', criteria_checked: [] },
    ],
    [obs('t1', 'todo', []), obs('t2', 'todo', [])],
  );
  assert.deepEqual(m, []);
});

test('a claim with no observation for its task is unverified (fail closed)', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done', criteria_checked: [0] }],
    [],
  );
  assert.equal(m.length, 1);
  assert.equal(m[0].kind, 'unverified');
  assert.match(m[0].message, /could not be read back/);
});

test('a non-array observed list is unverified for every claim', () => {
  const m = compareCloserClaimsWithDisk(
    [
      { task_id: 't1', status_set: 'done' },
      { task_id: 't2', status_set: 'review' },
    ],
    null,
  );
  assert.deepEqual(m.map((x) => [x.task_id, x.kind]), [['t1', 'unverified'], ['t2', 'unverified']]);
});

test('an observation that reports an error or has no status is unverified', () => {
  const m = compareCloserClaimsWithDisk(
    [
      { task_id: 't1', status_set: 'done' },
      { task_id: 't2', status_set: 'done' },
    ],
    [
      { task_id: 't1', status: '', checked_indices: [], error: 'task not found' },
      { task_id: 't2', checked_indices: [] },
    ],
  );
  assert.deepEqual(m.map((x) => x.kind), ['unverified', 'unverified']);
  assert.match(m[0].message, /task not found/);
});

test('claims with nothing checkable (unchanged, no criteria) need no observation', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'unchanged', criteria_checked: [] }],
    [],
  );
  assert.deepEqual(m, []);
});

test('task ids are opaque: t1 does not match t12', () => {
  const m = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done' }],
    [obs('t12', 'done', [])],
  );
  assert.equal(m[0].kind, 'unverified');
});

// ------------------------------------------------------------
// Requirement-link claims (REQ-TRC-020, SPEC-TRC-012)
// ------------------------------------------------------------
// The closer leaves a task with no requirement link in `review` and says so
// with `requirement_link_missing: true`; the verifier reads `orphan` back.

test('a task left in review for a missing requirement link, orphan on disk, is not a mismatch', () => {
  const mismatches = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'review', criteria_checked: [0], requirement_link_missing: true }],
    [{ task_id: 't1', status: 'review', checked_indices: [0], orphan: true }],
  );
  assert.deepEqual(mismatches, []);
});

test('a missing-link claim for a task that is not an orphan on disk is a mismatch', () => {
  const mismatches = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'review', requirement_link_missing: true }],
    [{ task_id: 't1', status: 'review', checked_indices: [], orphan: false }],
  );
  assert.equal(mismatches.length, 1);
  assert.equal(mismatches[0].kind, 'requirement_link');
  assert.equal(mismatches[0].task_id, 't1');
  assert.equal(
    mismatches[0].message,
    'left in review for a missing requirement link, but the task is not an orphan on disk',
  );
});

test('a missing-link claim that the verifier could not confirm fails closed', () => {
  const mismatches = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'review', requirement_link_missing: true }],
    [{ task_id: 't1', status: 'review', checked_indices: [] }],
  );
  assert.equal(mismatches.length, 1);
  assert.equal(mismatches[0].kind, 'unverified');
  assert.equal(mismatches[0].message, 'requirement-link claim could not be read back from disk (no orphan value)');
});

test('a missing-link claim must come with status review', () => {
  const mismatches = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done', requirement_link_missing: true }],
    [{ task_id: 't1', status: 'done', checked_indices: [], orphan: true }],
  );
  const kinds = mismatches.map((m) => m.kind);
  assert.deepEqual(kinds, ['requirement_link']);
  assert.equal(
    mismatches[0].message,
    'requirement link missing but status_set is done (expected review)',
  );
});

test('a task claimed done that is an orphan on disk is a mismatch', () => {
  const mismatches = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'done', criteria_checked: [0] }],
    [{ task_id: 't1', status: 'done', checked_indices: [0], orphan: true }],
  );
  assert.equal(mismatches.length, 1);
  assert.equal(mismatches[0].kind, 'requirement_link');
  assert.equal(
    mismatches[0].message,
    'status done but the task has no requirement link on disk (expected review)',
  );
});

test('orphan on disk is only held against a done claim, and a false or absent orphan against none', () => {
  // Left in review for another reason (a pending follow-up): the orphan state is not the closer's claim.
  assert.deepEqual(
    compareCloserClaimsWithDisk(
      [{ task_id: 't1', status_set: 'review' }],
      [{ task_id: 't1', status: 'review', checked_indices: [], orphan: true }],
    ),
    [],
  );
  for (const orphan of [false, undefined, null, 'true']) {
    assert.deepEqual(
      compareCloserClaimsWithDisk(
        [{ task_id: 't1', status_set: 'done' }],
        [{ task_id: 't1', status: 'done', checked_indices: [], orphan }],
      ),
      [],
      `orphan=${String(orphan)}`,
    );
  }
});

test('requirement_link_missing false or absent changes nothing', () => {
  for (const flag of [false, undefined, null]) {
    assert.deepEqual(
      compareCloserClaimsWithDisk(
        [{ task_id: 't1', status_set: 'done', criteria_checked: [0], requirement_link_missing: flag }],
        [{ task_id: 't1', status: 'done', checked_indices: [0] }],
      ),
      [],
    );
  }
});

test('a missing-link claim on an unreadable task is reported once, as unverified', () => {
  const mismatches = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'review', requirement_link_missing: true }],
    [{ task_id: 't1', status: '', checked_indices: [], error: 'task not found' }],
  );
  assert.equal(mismatches.length, 1);
  assert.equal(mismatches[0].kind, 'unverified');
  assert.match(mismatches[0].message, /task not found/);
});

test('a status mismatch and a requirement-link mismatch are both reported', () => {
  const mismatches = compareCloserClaimsWithDisk(
    [{ task_id: 't1', status_set: 'review', requirement_link_missing: true }],
    [{ task_id: 't1', status: 'done', checked_indices: [], orphan: false }],
  );
  assert.deepEqual(mismatches.map((m) => m.kind), ['status', 'requirement_link']);
});
