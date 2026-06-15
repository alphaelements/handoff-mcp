---
name: handoff-load
description: "Load handoff context for the current project. Calls handoff_load_context, summarizes tasks/decisions/blockers, and shows what to work on next. Triggers on '/handoff-load', 'コンテキスト読み込み', 'what was I working on', 'resume work'."
user-invocable: true
---

# Handoff Load

セッション開始時にプロジェクトの引き継ぎコンテキストを読み込み、現状を把握する。

## 手順

1. `handoff_load_context` を呼ぶ（引数なし — cwd を使用）
2. `not_initialized` が返った場合:
   - ディレクトリ名からプロジェクト名を推測
   - `handoff_init` で初期化
   - 初期化完了を報告して終了
3. 返されたコンテキストを以下の順で確認・要約:
   - **前回セッション**: summary, branch, commit, ended_at
   - **タスク**: `blocked` → `in_progress` → `todo` の優先順で表示
   - **工数サマリー**: total_estimate_hours, total_actual_hours, completion_rate, overdue_count を報告
   - **期限超過**: overdue_count > 0 なら該当タスクを強調
   - **ブロッカー**: あれば強調表示
   - **決定事項**: confidence が `unverified` のものを警告
   - **申し送り**: `caution` を最初に、`context` → `suggestion` の順
   - **コンテキストポインタ**: 次に読むべきファイル一覧
4. ユーザーに現状サマリーを日本語で報告
5. 次のアクションを提案（「何から始めますか？」）
