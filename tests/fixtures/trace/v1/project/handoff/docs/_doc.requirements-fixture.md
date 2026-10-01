---
id: doc-20260927-035006-168397
title: Requirements
doc_type: note
tags: []
scope_paths: []
parent_id: null
children: []
related: []
auto_inject: auto
task_ids: []
layer: requirement
source:
  origin: authored
  canonical_hash: 2dcbddfc42fcb834
  body_raw_hash: 18af62fdf0844976
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-09-27T03:50:06.168288270+00:00
updated_at: 2026-09-27T03:50:06.168288270+00:00
content_hash: 2dcbddfc42fcb834
verification:
  status: pending
  created_at: 2026-09-27T03:50:06.168288270+00:00
  updated_at: 2026-09-27T03:50:06.193000302+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: Requirements
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Account lockout
      status: pending
      notes: ''
      category: requirement
      stable_id: REQ-001
      priority: P0
      dev_stage: not_started
      task_ids:
      - t-fixture-1
      origin: body
      body_hash: cd4fb9a691d29a03
    - index: 1
      description: Session timeout
      status: pending
      notes: ''
      category: requirement
      stable_id: REQ-002
      priority: P1
      dev_stage: not_started
      origin: body
      body_hash: 2126b964cc61ee7e
---
# Requirements

### REQ-001 Account lockout

- priority: P0

After 5 consecutive failed logins, the account locks for 15 minutes.

### REQ-002 Session timeout

- priority: P1

Idle sessions expire after 30 minutes of inactivity.
