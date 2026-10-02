---
id: doc-20261001-060415-835799
title: Requirements v2
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
  canonical_hash: 6927f74709213a10
  body_raw_hash: 50076a675afcf5e5
  layer_sync_stamp: 2:18a1ac254a3d8e81
  content_hash_scheme: 1
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-10-01T06:04:15.835786143+00:00
updated_at: 2026-10-01T06:04:16.041335351+00:00
content_hash: 6927f74709213a10
verification:
  status: pending
  created_at: 2026-10-01T06:04:15.835786143+00:00
  updated_at: 2026-10-01T06:04:16.041335351+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: Requirements v2
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Export throttling
      status: pending
      notes: ''
      category: requirement
      stable_id: REQ-002
      priority: P1
      dev_stage: not_started
      task_ids:
      - t-impl-throttle
      origin: body
      body_hash: 554ae902463264af
      def_hash: 1a92f923cc0133ae
      acceptance:
      - label: AC1
        kind: gwt
      - label: AC2
        kind: gwt
    - index: 1
      description: Audit trail
      status: pending
      notes: ''
      category: requirement
      stable_id: REQ-003
      priority: P1
      dev_stage: not_started
      origin: body
      body_hash: b0a262a678e983c3
      def_hash: b13220872edb5786
    - index: 2
      description: Legacy export format
      status: pending
      notes: ''
      category: requirement
      stable_id: REQ-005
      priority: P3
      dev_stage: not_started
      origin: body
      body_hash: 9bc1983b4ad58233
      def_hash: 846b95ed0989d943
      waivers:
      - axis: verify
        reason: Format is deprecated, manual review only
    - index: 3
      description: Webhook retries
      status: pending
      notes: ''
      category: requirement
      stable_id: REQ-007
      priority: P2
      dev_stage: not_started
      origin: body
      body_hash: 3c96b54830e53085
      def_hash: 9d9c9419f015569f
---
# Requirements v2

### REQ-002 Export throttling

- priority: P1

Exports are rate-limited per account.

受入基準:
- AC1: Given 10 exports in 60 seconds When an 11th is requested within that window Then it is rejected with an error
- AC2: Given a rejected export When the minute elapses Then the next export succeeds

### REQ-003 Audit trail

- priority: P1

Every export is recorded in the audit log.

### REQ-005 Legacy export format

- priority: P3
- waive-verify: Format is deprecated, manual review only

The legacy CSV export format remains readable.

### REQ-007 Webhook retries

- priority: P2

Failed webhook deliveries are retried with exponential backoff and jitter.
