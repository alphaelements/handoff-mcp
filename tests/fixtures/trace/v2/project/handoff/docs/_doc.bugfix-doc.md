---
id: doc-20261001-060415-809150
title: Bugfix repro
doc_type: note
tags: []
scope_paths: []
parent_id: null
children: []
related: []
auto_inject: auto
task_ids: []
layer: requirement
trace_profile: bugfix
source:
  origin: authored
  canonical_hash: 30a16ac2e97e58fb
  body_raw_hash: 14584c9fa1a1d4f2
  layer_sync_stamp: 2:18a1ac254a3d8e81
  content_hash_scheme: 1
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-10-01T06:04:15.809124552+00:00
updated_at: 2026-10-01T06:04:15.809124552+00:00
content_hash: 30a16ac2e97e58fb
verification:
  status: pending
  created_at: 2026-10-01T06:04:15.809124552+00:00
  updated_at: 2026-10-01T06:04:15.809124552+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: Bugfix repro
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Session timeout lockout
      status: pending
      notes: ''
      category: requirement
      stable_id: REQ-001
      priority: P0
      dev_stage: not_started
      origin: body
      body_hash: e8e1364f4a3dafa4
      def_hash: cc3a2b35aa2cec39
      acceptance:
      - label: AC1
        kind: gwt
      - label: AC2
        kind: gwt
      rationale: Session hijack mitigation
    - index: 1
      description: Given an active session When 30 minutes pass with no activity Then the session expires
      status: pending
      notes: ''
      category: check
      stable_id: REQ-001#AC1
      dev_stage: not_started
      origin: body
      layer: acceptance
      verifies:
      - REQ-001#AC1
      def_hash: 61ac9c52d4e2d378
      implicit_of: REQ-001
      link_baselines:
        REQ-001#AC1: 61ac9c52d4e2d378
    - index: 2
      description: Given an expired session When it is reused Then re-authentication is required
      status: pending
      notes: ''
      category: check
      stable_id: REQ-001#AC2
      dev_stage: not_started
      origin: body
      layer: acceptance
      verifies:
      - REQ-001#AC2
      def_hash: 4b4506c5e215e725
      implicit_of: REQ-001
      link_baselines:
        REQ-001#AC2: 4b4506c5e215e725
---
# Bugfix repro

### REQ-001 Session timeout lockout

- priority: P0
- rationale: Session hijack mitigation

Sessions expire after 30 minutes idle.

受入基準:
- AC1: Given an active session When 30 minutes pass with no activity Then the session expires
- AC2: Given an expired session When it is reused Then re-authentication is required
