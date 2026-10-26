---
name: gui-vrt
description: "GUI visual regression testing (VRT) pipeline — mock -> golden -> verify. Captures golden screenshots from HTML mocks, compares the real UI (code-server + Playwright MCP) against them with pixelmatch, or asks the multimodal LLM to judge two screenshots (ai-compare), and records pass/fail in the trace graph. Triggers on 'GUI検証', 'GUI 検証', 'VRT', 'ビジュアルリグレッション', 'ゴールデン', 'ゴールデンスクリーンショット', 'モックと実装を比較', 'スクリーンショット比較', '視覚検証', 'gui-vrt', 'golden screenshot', 'visual regression', 'visual diff', 'pixelmatch', 'capture-golden', 'ai-compare', or when a requirement carries a `mock` attribute."
---

# gui-vrt — GUI Visual Regression Pipeline

Turns an HTML mock into a golden screenshot, then checks the implemented GUI
against that golden and writes the verdict into the trace graph. Specification:
REQ-GUI-001..006 (requirements) and SPEC-GUI-001..005 (basic specification).

| need | read |
|---|---|
| Installing Playwright / pixelmatch / code-server, CI with xvfb, `[gui]` config | [references/setup-guide.md](references/setup-guide.md) |
| The pixel comparison helper (copy into the project) | [references/vrt-compare.mjs](references/vrt-compare.mjs) |
| `ai-compare` verdict agent | `gui-visual-verifier` agent (`plugin-task-loop/agents/gui-visual-verifier.md`) |
| Recording tools (`trace_record`, `trace_test_run`) | the `handoff-trace` skill, `references/tools.md` |
| Starting and driving code-server for the real UI | the `code-server-gui` skill (if installed) |

## 1. Pipeline

```
Phase 1  mock          requirement item carries `- mock: <path>`     (authoring)
Phase 2  capture-golden  Playwright MCP screenshots the mock
                         -> user approves -> stored as golden        (this skill)
Phase 3  verify          code-server + Playwright MCP screenshots the
                         real UI -> pixelmatch vs golden
                         -> trace_record pass/fail + diff evidence   (this skill)
         compare         two PNGs in, diff image + similarity_pct out
         ai-compare      two PNGs in, LLM visual verdict out (no golden store needed)
```

Choose the mode by what you have:

| situation | mode |
|---|---|
| Mock exists, no golden yet, design is approved | `capture-golden` |
| Golden exists, implementation changed | `verify` (pixel) |
| Two arbitrary screenshots, you only want numbers | `compare` |
| Mock vs implementation, fonts/rendering differ so pixel diff is too strict, or no golden | `ai-compare` |

Ask the user which mode if the request does not say. Default for "check the GUI
against the mock" is `verify` when goldens exist, otherwise `ai-compare`.

## 2. Conventions (shared by every mode)

### 2.1 What is "a feature" and "a version"

- **feature**: a slug for one screen or component, lower-case `[a-z0-9-]`
  (for example `dashboard`, `task-detail`). Derive it from the mock path's
  directory/file name unless the requirement already names one.
- **version**: the golden generation, `v1`, `v2`, ... A new version is created
  whenever the approved design changes; old versions are never overwritten. The
  pair `(feature, version)` is unique.

### 2.2 File layout (project root relative)

```
.handoff/evidence/golden/<feature>/<version>/<name>.png   golden (from the mock)
.handoff/evidence/actual/<feature>/<run>/<name>.png       screenshot of the real UI
.handoff/evidence/diff/<feature>/<run>/<name>.png         pixelmatch diff image
```

`<run>` is a UTC timestamp `YYYYMMDD-HHMMSS`. `<name>` is the viewport/state name
(`default`, `empty`, `dark`, ...); one mock may have several states.

Evidence paths handed to handoff tools must be **non-empty, project-root
relative, and must not contain `..`**, so always record the paths exactly as
above, never `../`-style or absolute paths.

> **Server support.** The server parses the `mock` / `golden` / `viewport` /
> `capture_method` attributes (REQ-GUI-REQ-001) and validates evidence entries:
> `path` (and every `vrt.*_path`) must be non-empty, project-relative and free of
> `..`. The golden directory itself is still managed by this skill with Bash
> (creating directories, copying files); the server stores no images.

### 2.3 Finding the requirements that have a mock

Use the parsed attributes, never a hand-written grep of the layer documents:

```
handoff_doc_req_list(limit: 100, offset: 0 [, milestone_filter?, category?, task_id?])
```

Each item carries `stable_id` and, when authored, `mock`, `golden`, `viewport` and
`capture_method` (`capture_method` already falls back to the `[gui]`
`default_capture_method`). The tool has no "has a mock" filter: keep the items whose
`mock` is set, and page (`offset += limit`) until `offset >= total`. Build a list
`[(stable_id, mock, golden?, viewport?, capture_method?)]`. If no item has a mock, stop
and tell the user; do not guess one.

### 2.4 Settings

Read the optional `[gui]` table from `.handoff/config.toml` (see the setup
guide). The skill, not the server, reads it. Defaults when a key or the whole
table is absent:

| key | default | meaning |
|---|---|---|
| `viewport_width` / `viewport_height` | `1280` / `800` | screenshot size; golden and actual **must** use the same |
| `threshold` | `0.1` | pixelmatch per-pixel colour tolerance (0..1) |
| `min_similarity` | `0.99` | `verify` passes when `similarity_pct >= min_similarity` |
| `code_server_url` | `http://localhost:8080` | where the real UI is served |
| `compare_script` | `scripts/vrt-compare.mjs` | pixel comparison helper |

### 2.5 Taking screenshots with Playwright MCP

Always: `browser_resize` to the configured viewport **first**, then
`browser_navigate`, wait for the page to settle (`browser_wait_for` on a stable
element or a short time), then `browser_take_screenshot` with a `filename`.
Playwright MCP writes the file under its own output directory, so move it to the
convention path with Bash (`mkdir -p` the target directory, then `mv`). Verify the
file exists and is non-empty before continuing.

For a **mock** (static HTML): navigate to `file://<absolute path to mock>`; the
page needs no login. For the **real UI**: see `verify` step 3. Remove volatile
content before shooting (clocks, banners, toasts, random ids), for example with
`browser_evaluate` hiding the elements, otherwise every run differs.

### 2.6 Recording results

Record **every** verdict. A verdict that exists only in the chat is lost. The whole
chain is MCP calls; no file under `.handoff/` is written by hand.

1. **Identify the verification item** that checks the requirement: the AT / ST item
   whose `verifies` points at the requirement (`handoff_trace_report`,
   `handoff_trace_next`). If none exists, record against the requirement's own
   `stable_id` and say so in the note (the server warns about unresolved items but
   still records).
2. **Open (or reuse) a campaign** once per run, so the evidence has a home:

   ```
   handoff_trace_test_run(action: "list")          # reuse an in_progress campaign for this feature set
   handoff_trace_test_run(action: "create", scope: {items: ["<AT-...>", ...]},
     label: "gui-vrt <mode> <run>")                # -> test_run_id
   ```

   `record_check` is rejected on a `completed` / `approved` campaign; reopen it with
   `set_status` first, or create a new one.
3. **Record the verdict and its evidence** per verification item:

   ```
   handoff_trace_test_run(action: "record_check", test_run_id, item_id: "<AT-...>",
     result: "pass"|"fail", note: "<mode>: <numbers or reason>; feature=<f> golden=<v>",
     evidence: [
       {path: "<golden png>", type: "golden",     caption: "<feature> <version>",
        vrt: {baseline_path: "<golden png>", viewport: "<WxH>"}},
       {path: "<actual png>", type: "vrt-actual", caption: "real UI <run>",
        vrt: {baseline_path: "<golden png>", actual_path: "<actual png>", viewport: "<WxH>"}},
       {path: "<diff png>",   type: "vrt-diff",   caption: "similarity 99.87%, min 99%",
        vrt: {baseline_path: "<golden png>", actual_path: "<actual png>", diff_path: "<diff png>",
              similarity_pct: 99.87, threshold: 99, viewport: "<WxH>", tool: "pixelmatch"}}])
   ```

   The `vrt` object is the server's `VrtEvidence`: every field is optional,
   `similarity_pct` and `threshold` are **percentages 0..100**, and paths are
   project-relative. Units differ from the helper and the config, so convert:
   `vrt.similarity_pct = similarity_pct * 100` (the helper prints a 0..1 ratio) and
   `vrt.threshold = min_similarity * 100`. The report verdict is
   `similarity_pct >= threshold`, so `vrt.threshold` is the pass limit
   (`min_similarity`), **not** pixelmatch's per-pixel `threshold`; the latter goes in
   the caption/note. Omit `similarity_pct` / `threshold` when the mode produced no
   numbers (`ai-compare`): the report then claims no numeric verdict.
4. **Mirror the verdict into the trace graph** with the same paths as evidence:

   ```
   handoff_trace_record(
     results: [{item: "<AT-...>", result: "pass"|"fail", note: "<same note>",
                evidence: ["<actual png>", "<diff png>"]}],
     executor_kind: "ai", executor_id: "gui-vrt", test_run_id: "<test_run_id>")
   ```

   `result` is `pass` or `fail` only: a mode that could not run (no golden, server
   down) is **not** a failure of the requirement, so do not record it; report it to
   the user instead (use `blocked` only when the user asks for the gap to be visible
   in the trace graph).
5. `handoff_trace_record` does not refresh `_trace_report.json`; call
   `handoff_trace_report` once after the batch.

## 3. Mode `capture-golden`

Input: optional feature / requirement id filter (default: every item found in 2.3).

1. **Collect** the `(stable_id, mock path)` pairs (2.3). Confirm each mock file
   exists (`Read` / `ls`); a missing mock is reported and skipped.
2. **Plan** the golden set: feature slug, next version (one above the highest
   existing `.handoff/evidence/golden/<feature>/v*`, or `v1`), and the states to
   shoot. Show the plan to the user.
3. **Shoot** each mock (2.5) into a **staging** path
   `.handoff/evidence/golden/<feature>/<version>.pending/<name>.png`.
4. **Approval gate.** Show the user each screenshot (`Read` the PNG so it is
   displayed) and ask for approval, per feature. Golden is the reference for every
   future run; never promote it silently. On "no", delete the staging directory and
   stop for that feature (do not retry with changed settings unasked).
5. **Promote** approved sets: `mv ...<version>.pending ...<version>`. Never
   overwrite an existing version directory.
6. **Register.** Write `.handoff/evidence/golden/<feature>/<version>/meta.json`
   with `{"feature", "version", "requirement": "<stable_id>", "mock": "<path>",
   "viewport": "WxH", "captured_at": "<UTC ISO-8601>", "approved_by": "<user>"}`.
   When a campaign is open, attach the goldens with `handoff_trace_test_run`
   `add_evidence` (`type: "golden"`, caption `<feature> <version>`), which leaves
   the verdict unchanged. A golden capture is not a pass/fail verdict, so do **not**
   call `trace_record` here.
7. **Report**: list of feature, version, files, and what was skipped and why.

## 4. Mode `verify`

Input: optional feature / requirement filter.

The requirement discovery and the recording run entirely on MCP tools
(`handoff_doc_req_list` -> `handoff_trace_test_run` -> `handoff_trace_record`); Bash
is only used to take screenshots and run the pixel helper.

1. **Preconditions** (check, do not install anything silently): the compare helper
   exists at `compare_script`; `node_modules/pixelmatch` and `pngjs` resolve;
   code-server answers at `code_server_url`
   (`curl -s -o /dev/null -w "%{http_code}" <url>` is 200 or 302). Anything
   missing -> stop and point to [references/setup-guide.md](references/setup-guide.md).
2. **Extract** the requirements with `handoff_doc_req_list` (2.3). Read each item's
   `mock`, `golden`, `viewport` and `capture_method` attributes. Apply the user's
   filter. A requirement with a mock but no golden (neither a `golden` attribute that
   exists on disk nor a `.handoff/evidence/golden/<feature>/v*`) is reported as
   "no golden: run capture-golden" and skipped (not a failure). The golden is the
   `golden` attribute when set, otherwise the **highest** version of the feature
   unless the user named one.
3. **Shoot the real UI** with the item's `capture_method`:
   - a name configured as `[gui.capture_methods.<name>]` in `.handoff/config.toml`
     (or `default_capture_method`): run its `command` with Bash and collect the PNG it
     produces;
   - unset, or `playwright`: code-server + Playwright MCP (see the `code-server-gui`
     skill: it handles the `VSCODE_IPC_HOOK_CLI` pitfall and never kills other
     servers). `browser_resize` to the viewport, navigate to the screen under test,
     put it in the same state as the golden's `<name>`, clean volatile UI (2.5).

   Viewport precedence: the item's `viewport` (`WxH`), then the golden's `meta.json`,
   then `[gui]` defaults. If the item's viewport differs from the golden's, report the
   golden as stale instead of resizing silently. Screenshot to
   `.handoff/evidence/actual/<feature>/<run>/<name>.png`.
4. **Compare** every state with the helper:

   ```bash
   node <compare_script> <golden png> <actual png> <diff png> --threshold <threshold>
   ```

   It prints one JSON object (`similarity_pct` 0..1, `mismatched_pixels`,
   `size_mismatch`, ...). Exit code 2 means the comparison itself failed: report
   the error, record nothing.
5. **Judge**: `pass` when not `size_mismatch` and `similarity_pct >= min_similarity`;
   otherwise `fail`. A size mismatch is a `fail` with the note "viewport/size
   differs: expected AxB, actual CxD" (check first that the resize really applied
   before accepting that as an implementation regression). A feature passes only if
   **all** its states pass.
6. **Record** per feature with 2.6: one `handoff_trace_test_run` campaign for the run,
   a `record_check` carrying the golden / `vrt-actual` / `vrt-diff` evidence with the
   `vrt` object (percent units), then `handoff_trace_record` with the same verdict,
   then `handoff_trace_report`. Never skip the record because the verdict is a pass.
7. **Report** a table: requirement, feature, golden version, similarity,
   min_similarity, verdict, diff path, `test_run_id`. For each `fail`, `Read` the diff
   image and describe in one line where it differs. Offer `ai-compare` for failures
   that may be rendering noise; do not downgrade a `fail` yourself.

## 5. Mode `compare`

Input: two PNG paths (expected, actual) and optionally a diff output path
(default `.handoff/evidence/diff/adhoc/<run>/diff.png`).

1. Run the helper (same command as `verify` step 4).
2. Return `similarity_pct`, `mismatched_pixels`, `size_mismatch`, and the diff
   image path. `Read` the diff image so the user can see it.
3. Do **not** record to the trace graph unless the user links it to a requirement
   or verification item; if they do, use 2.6.

## 6. Mode `ai-compare`

Semi-automatic verification (REQ-GUI-006, SPEC-GUI-005): a multimodal model judges
two screenshots instead of counting pixels. Use it when pixel comparison is too
strict (fonts, antialiasing, dynamic data), when no golden exists, or to explain a
`verify` failure.

1. Obtain the **expected** image (an approved golden, or a fresh screenshot of the
   mock, 2.5) and the **actual** image (screenshot of the real UI, `verify` step 3).
   Same viewport for both.
2. Delegate the judgement to the `gui-visual-verifier` agent: pass it both absolute
   paths, the requirement text (and acceptance criterion) the screen must satisfy,
   and the state name. If the agent cannot be spawned, do the same job inline: `Read`
   both images and apply the criteria below.
3. The verdict follows SPEC-GUI-005:
   - `pass`: layout, colours and text are substantially identical; tiny differences
     (antialiasing, sub-pixel offsets) are tolerated.
   - `fail`: broken layout, missing or extra elements, wrong text, or a large colour
     difference.
   The agent returns `verdict`, a `reason` naming the concrete differences, and a
   confidence level. A `low` confidence verdict is not recorded as pass: ask the
   user. Verdict `ERROR` (unreadable input) is neither pass nor fail: fix the input
   and re-run, record nothing.
4. **Record** with 2.6, forwarding the agent's `record` payload (its `result`,
   `note` and `evidence`) to `handoff_trace_test_run(action: "record_check")` and then
   `handoff_trace_record`:
   - evidence: the expected image as `type: "golden"` (caption `expected (mock)`) and
     the actual image as `type: "vrt-actual"` (caption `actual`), each with
     `vrt: {baseline_path, actual_path, viewport, tool: "gui-visual-verifier"}`,
     project-relative. If the agent was given absolute paths, convert them back
     (strip the project root) before recording; a path outside the project is copied
     under `.handoff/evidence/` first.
   - `note`: `ai-compare: <verdict> (confidence <level>): <reason>`. The pass/fail,
     confidence and reason therefore reach the trace graph and the campaign.
   - `executor_id: "gui-visual-verifier"`. A `fail` must carry its reason; a record
     without it is not acceptable. No `similarity_pct` / `threshold` (there is no
     number to claim).

## 7. Rules

- Never promote a golden without explicit user approval; never overwrite a golden
  version.
- Never install packages or start long-lived servers without telling the user what
  and where; follow the setup guide, and never run `npm install` in a directory
  shared with another OS's `node_modules`.
- Never kill `vscode-server` / run `pkill -f code-server`; stop a standalone
  code-server only by the PID you started.
- Do not weaken `min_similarity` or `threshold` to turn a `fail` into a `pass`.
  Changing them is a user decision recorded in `[gui]`.
- Report what was skipped (no mock, no golden, tool missing) explicitly; skipped is
  not passed.
