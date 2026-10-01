---
id: doc-20261001-060415-852733
title: Basic spec v2
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
  canonical_hash: 04ae08ea7817b4fe
  body_raw_hash: ea456e7036d4bf7a
  layer_sync_stamp: 2:18a1ac254a3d8e81
  content_hash_scheme: 1
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-10-01T06:04:15.852720946+00:00
updated_at: 2026-10-01T06:04:15.852720946+00:00
content_hash: 04ae08ea7817b4fe
verification:
  status: pending
  created_at: 2026-10-01T06:04:15.852720946+00:00
  updated_at: 2026-10-01T06:04:15.852720946+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: Basic spec v2
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Audit log schema
      status: pending
      notes: ''
      category: requirement
      stable_id: SPEC-003
      dev_stage: not_started
      origin: body
      refines:
      - REQ-003
      body_hash: d44526e2234347bd
      def_hash: e71e2639ec9fbbfb
      link_baselines:
        REQ-003: b13220872edb5786
---
# Basic spec v2

### SPEC-003 Audit log schema

- refines: REQ-003

Audit entries store actor, action, and timestamp.
