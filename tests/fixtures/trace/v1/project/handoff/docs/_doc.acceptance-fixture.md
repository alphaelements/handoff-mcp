---
id: doc-20260927-035006-180342
title: Acceptance tests
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
  canonical_hash: 0bcf3a64553f2230
  body_raw_hash: f693fbb68b27bb45
has_bom: false
line_ending: lf
split_level: 2
created_at: 2026-09-27T03:50:06.180338964+00:00
updated_at: 2026-09-27T03:50:06.180338964+00:00
content_hash: 0bcf3a64553f2230
verification:
  status: pending
  created_at: 2026-09-27T03:50:06.180338964+00:00
  updated_at: 2026-09-27T03:50:06.180338964+00:00
  items:
  - fragment_seq: 0
    heading: ''
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
  - fragment_seq: 1
    heading: Acceptance
    status: pending
    impl_refs: []
    test_refs: []
    notes: ''
    category: section
    sub_items:
    - index: 0
      description: Lockout after 5 failures
      status: pending
      notes: ''
      category: check
      stable_id: AT-001
      dev_stage: not_started
      origin: body
      verifies:
      - REQ-001
      method: manual
      body_hash: c56e7946b9cd575c
---
# Acceptance

### AT-001 Lockout after 5 failures

- verifies: REQ-001
- method: manual

Fail login 5 times in a row, then confirm the account is locked for 15 minutes.
