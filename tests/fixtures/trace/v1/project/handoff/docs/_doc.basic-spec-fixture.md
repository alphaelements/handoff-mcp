---
id: doc-20260927-035006-175373
title: Basic spec
doc_type: note
tags: []
scope_paths: []
parent_id: null
children: []
related: []
auto_inject: auto
task_ids: []
layer: basic_spec
source:
  origin: authored
  canonical_hash: 04bac576b5b25169
  body_raw_hash: 4a3f7a20ce438304
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-09-27T03:50:06.175369305+00:00
updated_at: 2026-09-27T03:50:06.175369305+00:00
content_hash: 04bac576b5b25169
verification:
  status: pending
  created_at: 2026-09-27T03:50:06.175369305+00:00
  updated_at: 2026-09-27T03:50:06.175369305+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: Basic spec
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Lockout counter
      status: pending
      notes: ''
      category: requirement
      stable_id: SPEC-001
      dev_stage: not_started
      origin: body
      refines:
      - REQ-001
      body_hash: 4ff9c972bef11f5b
---
# Basic spec

### SPEC-001 Lockout counter

- refines: REQ-001

Maintain a per-account failed-login counter, reset on success.
