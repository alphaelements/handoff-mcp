---
id: doc-20260927-035006-185085
title: System tests
doc_type: note
tags: []
scope_paths: []
parent_id: null
children: []
related: []
auto_inject: auto
task_ids: []
layer: system_test
source:
  origin: authored
  canonical_hash: e4f7089e60fd7cd5
  body_raw_hash: 5d9504d19eb276b2
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-09-27T03:50:06.185081325+00:00
updated_at: 2026-09-27T03:50:06.185081325+00:00
content_hash: e4f7089e60fd7cd5
verification:
  status: pending
  created_at: 2026-09-27T03:50:06.185081325+00:00
  updated_at: 2026-09-27T03:50:06.185081325+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: System tests
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Counter increments on failure
      status: pending
      notes: ''
      category: check
      stable_id: ST-001
      dev_stage: not_started
      origin: body
      verifies:
      - SPEC-001
      method: auto
      body_hash: 746f4fc518e84e2a
---
# System tests

### ST-001 Counter increments on failure

- verifies: SPEC-001
- method: auto

Assert the failed-login counter increments by 1 on each failed login attempt.
