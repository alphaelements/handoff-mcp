# User Review Board

The review board is one plain Markdown file, `.handoff/review_board.md`, where
the AI puts questions and material for the user to review. The user answers in
any editor or in the Markdown preview: tick the checkbox, write a memo, or edit
an attribute. There is no frontmatter; `##` headings are categories and `###`
headings are items.

## Layout of an item

```markdown
### Choose the storage layout <!-- rb-id: R-001 -->
- priority: high
- assignee: ryoma
- task_id: t12
- date: 2026-11-17

Option A keeps everything in one file.

![mockup.png](review_board_assets/mockup.png)

- [ ] 確認済み

メモ:
```

- The stable id (`R-001`, one more than the highest id used so far) lives in an
  HTML comment on the heading line, so it is invisible in a preview.
- `priority`, `assignee`, `task_id` and `doc_id` appear only when given;
  `date` is always written. They are plain `- key: value` lines the user can
  edit.
- An item the user has answered has `- [x] 確認済み` or text after `メモ:`.

## Tools

| tool | use |
|---|---|
| `handoff_review_init()` | create the board with the default categories (設計判断, 実装確認, テスト確認, その他); an existing board is left byte-identical. Optional: `handoff_review_add` creates the board on first use |
| `handoff_review_add(title, category, body?, priority?, assignee?, image_paths?, task_id?, doc_id?)` | append an item at the end of the `##` section named `category` (created when missing); returns `{id, category, category_created, board_created, path}` |
| `handoff_review_list(status_filter?, assignee_filter?, category_filter?)` | list the items with `{id, title, status, priority, assignee, date, has_images, category}` plus `{total, pending, answered}` counts, without loading the whole board; filters combine with AND, a missing board gives an empty list |
| `handoff_review_read(id? \| seq?, format?)` | read ONE item section without loading the whole board. `id` is the stable id (`R-001`), `seq` the 0-based index among `###` items (`id` wins when both are given). Returns `{seq, id, title, category, body, status, priority, assignee, date, task_id, doc_id, images[], checkbox_checked, memo, content_hash}`; `format: "meta"` omits `body` and `memo`. `status` is `answered` once the user ticked the checkbox or wrote a memo, else `pending`. An unknown id, an out-of-range `seq`, neither argument, or a missing board is an error |
| `handoff_review_update(id, expected_hash, updates)` | rewrite ONE item section under an optimistic lock; every other byte of the file is kept exactly and the file is replaced atomically. `updates` keys (at least one): `body`, `priority`, `assignee` (empty string removes the line), `checkbox_checked`, `memo` (empty clears it), `images` (replaces the body's image links; empty array removes them). Returns `{id, updated[], status, checkbox_checked, content_hash}` |

## Reading and updating one item

1. `handoff_review_read({id: "R-001"})` and keep its `content_hash`.
2. `handoff_review_update({id: "R-001", expected_hash: <content_hash>, updates: {...}})`.
3. If the user edited that item in the meantime, the call fails, writes nothing,
   and the error carries `current_hash`. Re-read the item (its body may have
   changed), reapply your change, and retry with the new hash. Edits to other
   items never cause a mismatch.

The `content_hash` returned by an update is the hash for the next update, so a
chain of updates needs no re-read. `handoff_review_update` rejects, and writes
nothing for, text that would restructure the board: a `##` or `###` heading
line, an unclosed code fence, or a checkbox line inside `memo`. The heading and
its `rb-id` comment never change. A resolved item has already left the board,
so it is never returned.

`handoff_review_add` rejects, and writes nothing for, a title, category or
attribute that spans several lines, a title containing `<!--` or `-->`, a body
with a `##` or `###` heading line (use `####` or bold instead), and a body with
an unclosed code fence. Image paths are embedded as `![name](path)` links after
the body: `review_board_assets/x.png` for a file under
`.handoff/review_board_assets/`, or `../design/x.png` for a project-relative
file, both as seen from `.handoff/`.
