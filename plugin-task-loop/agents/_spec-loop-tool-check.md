# spec-loop third-party tool check (shared)

Shared procedure for the spec-loop coordinator. Run it **at the very start of Phase 0**,
before importing the brainstorm report, and show the result to the user. It never blocks
silently: the user decides whether to install, continue degraded, or stop.

This file is referenced by the spec-loop coordinator; the spec-loop agents rely on its
result (`mmdc` availability decides whether their Mermaid diagrams can be validated).

## Tools

| Tool | Role in spec-loop | Class |
|---|---|---|
| Mermaid CLI (`mmdc`) | validates and renders every Mermaid diagram (Phases 1-3) to SVG | **required** |
| chrome-headless-shell (or Google Chrome / Chromium) | headless browser that `mmdc` drives | **required** whenever `mmdc` is (it is a dependency of `mmdc`) |
| Typst CLI (`typst`) | compiles the specification to PDF (Phase 4 `doc_export_pdf`); release binaries of handoff-mcp have Typst built in, so the CLI is only used by a build without the `bundled-typst` feature | **optional** - only needed when the user wants a PDF and the handoff-mcp build has no built-in Typst |

This classification is spec-loop's own: the PDF export itself treats Typst as required (unless built in) and
`mmdc` as optional, but spec-loop without `mmdc` cannot verify or render its diagrams,
while spec-loop without Typst only loses the PDF.

## Detection

Run the checks with Bash. Each tool can be pinned by an environment variable, which wins
over `PATH`:

```bash
bin="${HANDOFF_MMDC_BIN:-$(command -v mmdc)}";        [ -n "$bin" ] && "$bin" --version
bin="${HANDOFF_TYPST_BIN:-$(command -v typst)}";      [ -n "$bin" ] && "$bin" --version
for c in "${HANDOFF_CHROME_BIN:-}" chrome-headless-shell google-chrome google-chrome-stable chromium chromium-browser; do
  [ -n "$c" ] && command -v "$c" >/dev/null 2>&1 && { "$c" --version; break; }
done
```

A tool is **installed** when its command runs and prints a version; otherwise it is
**missing**. Minimum versions (same as the PDF export): Typst >= 0.12.0, mmdc >= 10.0.0;
no minimum for the browser. A tool below its minimum is reported as **too old**, with the
same install guidance.

## Report to the user

Show one table, then act on it:

```
Tool                    Status         Needed for            Class
mmdc                    ok 11.4.0      diagram render        required
chrome-headless-shell   missing        mmdc (browser)        required
typst                   missing        PDF output only       optional
```

- Everything installed: say so in one line and continue.
- A **required** tool is missing or too old: list it with the guidance below, then ask the
  user (AskUserQuestion) whether to (a) pause while they install it, (b) continue without
  diagram validation (diagrams stay as unverified Mermaid text and Phase 4 notes it), or
  (c) stop. Do not install anything yourself.
- Only the **optional** tool is missing: if `handoff_doc_export` works without it (a release
  binary has Typst built in), say nothing more; otherwise continue; say that the specification will be
  written as Markdown/Mermaid and that PDF output is unavailable until Typst is installed.
- Offer to re-run the check after the user installs something.

## Install guidance and licenses

Show the entry for each missing tool. Each tool runs as a separate process the user
installs; nothing is bundled with handoff-mcp. Full license record:
`THIRD_PARTY_LICENSES` in the handoff-mcp repository (Rust crates: `THIRD_PARTY_LICENSES.md`).

**Mermaid CLI (`mmdc`)** - required. License: MIT (dependency puppeteer: Apache-2.0).
```
npm install -g @mermaid-js/mermaid-cli
```

**chrome-headless-shell** - required by `mmdc`. License: Chromium, BSD-3-Clause plus
bundled third-party licenses (Puppeteer, which downloads it, is Apache-2.0).
```
npx puppeteer browsers install chrome-headless-shell
```
or install Google Chrome / Chromium with the OS package manager, and set
`HANDOFF_CHROME_BIN` if it is not on `PATH`.

**Typst CLI (`typst`)** - optional (PDF output only). License: Apache-2.0.
```
macOS:    brew install typst
Linux:    snap install typst   (or a release archive from https://github.com/typst/typst/releases)
Windows:  winget install --id Typst.Typst
Any OS:   cargo install --locked typst-cli
```

Japanese text in the PDF also needs a Japanese font installed on the system (for example
Noto Sans CJK JP, SIL OFL 1.1); handoff-mcp does not ship one.
