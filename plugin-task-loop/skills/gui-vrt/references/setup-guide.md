# gui-vrt setup guide

Follow the steps in order on a project that has no GUI verification yet. When
step 6 passes, `gui-vrt verify` works (REQ-GUI-005 / SPEC-GUI-004).

Requirements: Node.js 20+ (for the pixel helper), a Chromium-capable machine, and
the Playwright MCP server enabled in your Claude Code setup (it provides the
`browser_*` tools the skill uses). code-server is only needed to verify a VSCode
extension's WebView; a plain web app can use its own dev server URL.

> **Shared working tree warning.** If `node_modules/` is shared between a Mac host
> and a Linux container (bind-mounted project directory), do **not** run `npm
> install` from the container: it overwrites the other OS's native bindings. Install
> on the side that owns `node_modules/`, or use a container-local overlay for it.
> `pixelmatch` and `pngjs` are pure JavaScript, but Playwright is not.

## 1. Playwright

```bash
npm install -D @playwright/test
npx playwright install chromium
```

On a minimal Linux image also install the browser's system libraries:
`npx playwright install-deps chromium` (needs root).

The skill itself drives the browser through **Playwright MCP**. Installing the
package and the Chromium build above is what makes that MCP server (and any
Playwright script you add later) start. Check: `npx playwright --version`.

## 2. pixelmatch

```bash
npm install -D pixelmatch pngjs
```

Then copy the comparison helper into the project (it must live inside the project
so that `pixelmatch` resolves from the project's `node_modules`):

```bash
mkdir -p scripts
cp <plugin dir>/skills/gui-vrt/references/vrt-compare.mjs scripts/vrt-compare.mjs
```

The helper is an ES module. Make sure Node treats `.mjs` as ESM (it always does);
the project's `package.json` does not need `"type": "module"`.

Check with any two PNGs:

```bash
node scripts/vrt-compare.mjs a.png b.png /tmp/diff.png
# {"similarity_pct":1,"mismatched_pixels":0,"total_pixels":...,"size_mismatch":false,...}
```

`similarity_pct` is a ratio from 0.0 to 1.0 (1.0 = identical). Exit code 0 means
the comparison ran; exit code 2 means bad arguments or an unreadable PNG.

## 3. code-server

Skip this step if you verify a plain web app (set `code_server_url` to its URL).

```bash
# install (once): https://github.com/coder/code-server  (or: npm i -g code-server)
code-server --disable-workspace-trust &
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:8080/    # 200 or 302 = up
```

- `--disable-workspace-trust` keeps Restricted Mode from disabling custom editors
  and WebViews.
- If you run it from a terminal inside a VSCode Remote-SSH session, start it with
  `env -u VSCODE_IPC_HOOK_CLI -u VSCODE_CWD code-server ...`; otherwise it hands the
  request to the already-running VSCode server and exits immediately.
- Install the extension under test into code-server:
  `code-server --install-extension path/to/extension.vsix`
  (extensions land in `~/.local/share/code-server/extensions/`). Rebuild and
  re-install after every change you want to verify.
- Config (bind address, auth) is `~/.config/code-server/config.yaml`. For
  password auth the skill types the password into the login page; for unattended
  runs prefer `auth: none` on a loopback-only `bind-addr: 127.0.0.1:8080`.
- To stop it, `kill` the PID you recorded when you started it. Never `pkill -f
  code-server`: that also kills the VSCode Remote-SSH server.

## 4. CI (headless, xvfb)

Headless Chromium needs no display, but code-server flows and headed browsers on
a Linux runner do. Wrap the run in `xvfb-run`:

```bash
sudo apt-get update && sudo apt-get install -y xvfb
xvfb-run --auto-servernum --server-args="-screen 0 1920x1080x24" \
  <command that runs your Playwright script, or Claude Code with the Playwright MCP>
```

Pin what makes screenshots differ between machines, otherwise goldens captured
locally fail in CI:

- Same viewport (`[gui] viewport_*`) and device scale factor 1.
- Same fonts: install the fonts the UI uses in the CI image (for example
  `fonts-noto-cjk`), or compare with `ai-compare` instead of pixels.
- Same Chromium build: keep `npx playwright install chromium` pinned to the
  `@playwright/test` version in `package-lock.json`.

Upload `.handoff/evidence/actual/` and `.handoff/evidence/diff/` as CI artifacts
so a failing run can be inspected.

## 5. `[gui]` section in `.handoff/config.toml`

Optional. The `gui-vrt` skill reads it; the handoff server ignores unknown tables
and preserves them when it edits the file through its own config tools. Edit the
file by hand to change these.

```toml
[gui]
viewport_width   = 1280        # golden and actual MUST use the same size
viewport_height  = 800
threshold        = 0.1         # pixelmatch per-pixel colour tolerance, 0..1
min_similarity   = 0.99        # verify passes when similarity_pct >= this (ratio 0..1)
code_server_url  = "http://localhost:8080"
compare_script   = "scripts/vrt-compare.mjs"
```

| key | default | note |
|---|---|---|
| `viewport_width`, `viewport_height` | 1280, 800 | Goldens remember their own size in `meta.json`; `verify` resizes to it |
| `threshold` | 0.1 | Lower = stricter per pixel. Anti-aliasing usually needs 0.1 or higher |
| `min_similarity` | 0.99 | Share of pixels allowed to be identical-or-within-threshold. Tune per project, record the reason in the commit |
| `code_server_url` | `http://localhost:8080` | Any URL serving the UI under test |
| `compare_script` | `scripts/vrt-compare.mjs` | Path relative to the project root |

Do not loosen `min_similarity` just to make a failing check pass.

## 6. Smoke test

1. Add a mock to a requirement item in a layer document:
   `- mock: webview-ui/mockup/dashboard.html` (any static HTML works).
2. `gui-vrt capture-golden` -> approve the screenshot ->
   `.handoff/evidence/golden/<feature>/v1/default.png` and `meta.json` exist.
3. Serve the real UI, run `gui-vrt verify` -> a `pass` / `fail` row appears in
   `handoff_trace_report` for the verification item, with the actual and diff
   screenshots as evidence.
4. Change a colour or remove an element in the UI and run `verify` again -> the
   verdict must be `fail` with a diff image highlighting the change. If it still
   passes, the viewport, state or threshold is wrong; fix the setup before relying
   on the pipeline.

## Troubleshooting

| symptom | cause / fix |
|---|---|
| `Cannot find package 'pixelmatch'` | the helper is not inside the project, or step 2 was skipped / done in another directory |
| `size_mismatch: true` | viewport differs from the golden's; the browser was not resized before navigating, or the page has a scrollbar the golden lacks |
| everything differs by a few pixels | fonts or scale factor differ between the capture machines (step 4); regenerate the golden on the CI-equivalent environment or use `ai-compare` |
| code-server exits at once | `VSCODE_IPC_HOOK_CLI` is set (step 3) |
| login page in every screenshot | code-server auth is on; use the login step of the `code-server-gui` skill or set `auth: none` on loopback |
| `Executable doesn't exist` from Playwright | `npx playwright install chromium` was not run on this machine / OS |
