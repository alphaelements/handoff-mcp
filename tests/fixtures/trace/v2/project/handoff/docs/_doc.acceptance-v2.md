---
id: doc-20261001-060415-871069
title: Acceptance v2
doc_type: note
tags: []
scope_paths: []
parent_id: null
children: []
related: []
auto_inject: auto
task_ids: []
layer: acceptance
source:
  origin: authored
  canonical_hash: 2b2d5623d9fe6b98
  body_raw_hash: 73c1884621b5c80f
  layer_sync_stamp: 2:18a1ac254a3d8e81
  content_hash_scheme: 1
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-10-01T06:04:15.871055106+00:00
updated_at: 2026-10-01T06:04:16.091711292+00:00
content_hash: 2b2d5623d9fe6b98
verification:
  status: pending
  created_at: 2026-10-01T06:04:15.871055106+00:00
  updated_at: 2026-10-01T06:04:16.091711292+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: Acceptance v2
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Throttling rejects the 11th export
      status: pending
      notes: ''
      category: check
      stable_id: AT-002
      dev_stage: not_started
      task_ids:
      - t-exec-throttle
      origin: body
      verifies:
      - REQ-002#AC1
      method: manual
      body_hash: bbcae0bb74da083a
      def_hash: 70d6361c993c83a6
      link_baselines:
        REQ-002#AC1: af271dbbc008c44c
    - index: 1
      description: Export audit entry recorded
      status: pending
      notes: ''
      category: check
      stable_id: AT-006
      dev_stage: not_started
      task_ids:
      - t-exec-webhook
      origin: body
      verifies:
      - REQ-007
      method: manual
      body_hash: e9085479f6b3c120
      def_hash: 4af6b8569593d26b
      link_baselines:
        REQ-007: c220c993671394f9
    - index: 2
      description: Legacy export still parses
      status: pending
      notes: ''
      category: check
      stable_id: AT-007
      dev_stage: not_started
      origin: body
      verifies:
      - REQ-007
      method: manual
      body_hash: 03d016c6e9a13d43
      def_hash: cc2e4121bf23556c
    - index: 3
      description: Exploratory session audit
      status: pending
      notes: ''
      category: check
      stable_id: ST-004
      dev_stage: not_started
      origin: body
      method: manual
      body_hash: 674ce65a33db61be
      def_hash: 261a2ee8d832387e
      derived: Exploratory check with no upstream link
---
# Acceptance v2

### AT-002 Throttling rejects the 11th export

- verifies: REQ-002#AC1
- method: manual

Confirm the 11th export within a minute is rejected.

### AT-006 Export audit entry recorded

- verifies: REQ-007
- method: manual

Confirm a webhook retry is recorded and the retry count is logged.

### AT-007 Legacy export still parses

- verifies: REQ-007
- method: manual

Placeholder verifier used to simulate a pre-M2 unbaselined link (see fixture README).

### ST-004 Exploratory session audit

- derived: Exploratory check with no upstream link
- method: manual

Exploratory check with no formal upstream requirement.
