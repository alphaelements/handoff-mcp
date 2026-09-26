# Contributing to handoff-mcp

## Local development gates

Before sending a change, run the same gates CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

### Performance gate (NFR-008 / NFR-009)

If your change touches the storage layer (`src/storage/`) or the MCP
handlers that read/write `.handoff/` (`src/mcp/handlers/`), also run the
Tier 1 performance budget harness at scales S and M. It drives a real
`handoff-mcp` binary over stdio JSON-RPC against a deterministic synthetic
project and checks the result against the latency/I/O budgets in
`tests/perf_budgets.toml` (see `wiki/240-performance-design.md` §6-7 for the
full design, and `wiki/240` §2 for what "S"/"M"/"L"/"JA" mean):

```bash
cargo test --release --test perf_budget -- --ignored --test-threads=1 --nocapture perf_budget_scale_s
cargo test --release --test perf_budget -- --ignored --test-threads=1 --nocapture perf_budget_scale_m
```

- `--release` is required — the budgets are calibrated against an optimized
  build; a debug build is routinely 5-10x slower and will fail spuriously.
- `--test-threads=1` is required — the harness reads a spawned server
  process's `/proc/<pid>/io` counters (Linux) and measures wall-clock
  latency, both of which get noisy under concurrent test threads.
- A budget marked `expected_fail = "reason"` in `tests/perf_budgets.toml` is
  a known, already-tracked gap (printed as `expected-fail`, non-fatal) — see
  the referenced task in `wiki/240-performance-design.md` §4. If your change
  is the fix for one of those and the table now prints `PROMOTE?` for that
  op, remove its `expected_fail` key so the budget is enforced again.
- `L` (3,000 tasks) and `JA` (Japanese body text) scales, and the PR-9
  scale-ratio checks, are nightly-only (`perf_budget_scale_l`,
  `perf_budget_scale_ja`, `perf_budget_scale_ratio_n`,
  `perf_budget_scale_ratio_d`) — slow enough (regenerating a large fixture
  from scratch) that they're not part of the local/CI dev gate, but you can
  run any of them the same way if you want a sanity check before a larger
  perf change.
- `HANDOFF_PERF_SLACK` (default `1.0`) multiplies every `ms` budget before
  comparison; CI sets it to `3.0` for its shared runners. Leave it unset
  locally unless your machine is unusually busy.

Tier 2 (criterion micro-benchmarks for specific hot functions — see
`benches/docs_read.rs`) has no pass/fail budget; it's for tracking a
before/after delta on a targeted change:

```bash
cargo bench --bench docs_read
```
