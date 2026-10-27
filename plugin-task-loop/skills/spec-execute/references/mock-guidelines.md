# HTML mock-up guidelines for spec-loop Phase 2

Rules for the `spec-gui-designer` agent and for the coordinator when reviewing its files
(REQ-SL-004). A mock shows layout, content and navigation; it is not a prototype and not
production code.

## Files

- One file per screen: `<output_dir>/<screen_id>.html`, `screen_id` from the brainstorm `screens`
  table (ASCII, lower-case, `-` separated is fine: `home.html`, `measure-detail.html`).
- Optional `<output_dir>/index.html`: a list of links to every screen, so the user opens one file.
- No other file types: styles are inline, there are no images or fonts to ship.

## Skeleton

Each mock is self-contained: no framework, no CDN, no `<script src>`, no web fonts, no network.
Start from this and adapt:

```html
<!DOCTYPE html>
<html lang="ja">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ホーム画面 (S_HOME)</title>
  <style>
    :root { --fg: #1a1a1a; --bg: #fafafa; --accent: #1f6feb; --border: #d0d7de; }
    * { box-sizing: border-box; }
    body { margin: 0; font-family: system-ui, sans-serif; color: var(--fg); background: var(--bg); }
    header, main, footer { max-width: 480px; margin: 0 auto; padding: 12px 16px; }
    nav a, button { display: inline-block; padding: 8px 12px; border: 1px solid var(--border);
      border-radius: 6px; background: #fff; color: var(--accent); text-decoration: none; }
    a:focus-visible, button:focus-visible, input:focus-visible { outline: 3px solid var(--accent); }
    .card { border: 1px solid var(--border); border-radius: 8px; padding: 12px; margin: 8px 0; background: #fff; }
    label { display: block; margin: 8px 0 4px; }
  </style>
</head>
<body>
  <header>
    <h1>ホーム</h1>
    <nav aria-label="メイン"><a href="settings.html">設定</a></nav>
  </header>
  <main>
    <a class="card" href="measure-detail.html">今日の測定: 36.5 °C</a>
    <p>最終更新: 10:42</p>
  </main>
  <footer><a href="login.html">ログアウト</a></footer>
</body>
</html>
```

## Content rules

- **Realistic placeholder content** in the user's language: plausible names, numbers, dates and
  messages, not lorem ipsum. Show the states the screen can be in: filled, empty, error, loading
  (as separate sections or extra screens listed under "Added screens").
- **Every main element from the screen definition appears**, and nothing the definition does not
  mention does without being listed in the agent's "Added screens / design decisions".
- Follow `ux_policy` (tone, density, platform, accessibility) and say in the report which points
  you applied.
- Semantic structure (`header`, `nav`, `main`, `h1`-`h3`, `label` bound to inputs, `button` for
  actions, `a` for navigation), visible focus styles, and sufficient contrast.
- Layout is simple CSS (flex or grid). Use a width that reflects the target platform (about 480px
  for mobile, 960px for desktop), not responsive breakpoints.

## Navigation

- Every control that moves to another screen is an `<a href="<other_screen_id>.html">` with a
  relative `href`, so the user can click through the mocks from a file browser.
- Each such control is exactly one transition in the `stateDiagram-v2`, labelled with the control
  text. Cross-check both directions: every `screen_flow` line appears as a control and an edge, and
  every control appears as an edge.
- Actions that stay on the screen (save, toggle) are `button`s and do not appear in the diagram
  unless they change the screen state (for example into an error state).

## Safety

- Text taken from the input (screen names, element labels, user wording) is placed into markup only
  after you escape it: `&` to `&amp;`, `<` to `&lt;`, `>` to `&gt;`, `"` to `&quot;`. Never splice
  raw input into attributes or `<style>`.
- No `<script>` that fetches, no `<iframe>`, no `<form action>` pointing to a real URL (use `#`),
  no remote `src` or `href` except relative links to the sibling mocks.
- Write only inside `output_dir`.

## Review checklist (coordinator, before the user sees the files)

1. A file exists for every screen id in the brief; open each with `Read` and check it starts with
   `<!DOCTYPE html>` and has `<style>` inline.
2. `grep -rn "http\|<script" <output_dir>` finds nothing unexpected.
3. The `href` targets all exist in `output_dir`.
4. The cross-check in the agent's report is `None`, or each mismatch is explained to the user.

## Optional: PNG for the PDF

When a headless browser is available (the Chrome found by the tool check):

```bash
google-chrome --headless --disable-gpu --screenshot=<out>.png --window-size=480,900 file://<abs path>.html
```

Then the specification embeds `![ホーム画面](<out>.png)`. Without a browser, list the HTML paths in
the specification and report that the PDF has no screen images.
