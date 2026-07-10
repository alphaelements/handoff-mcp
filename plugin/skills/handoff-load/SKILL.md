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
3. **paused セッション**がある場合:
   - `paused_sessions` をユーザーに表示（ID, summary, branch）
   - 再開したい場合: `handoff_load_context(session_id: "s-...")` で paused → active に遷移
   - 不要な場合: `handoff_save_context(close_session_id: "s-...")` で paused → closed に直接遷移
4. **`session_guidance` がある場合（アクティブセッション未確立）**:
   - 作業開始前に `handoff_save_context` を `session_status: "active"` で呼んでセッションを確立する
   - `session_guidance.suggested_fields` の内容（summary, decisions, context_pointers, references, related_task_ids）を引き継いで含める
   - `handoff_notes` に最低1つ `suggestion`（これから何をするか）を含める
   - これにより中断時にも「何をしようとしていたか」が `.handoff/` に残る
5. 返されたコンテキストを以下の順で確認・要約:
   - **前回セッション / previous_session**: summary, branch, commit, ended_at。引き継ぎ情報（decisions, handoff_notes, context_pointers）を確認
   - **タスク**: `blocked` → `in_progress` → `todo` の優先順で表示
   - **工数サマリー**: total_estimate_hours, total_actual_hours, completion_rate, overdue_count を報告
   - **期限超過**: overdue_count > 0 なら該当タスクを強調
   - **ブロッカー**: あれば強調表示
   - **決定事項**: confidence が `unverified` のものを警告
   - **申し送り**: `caution` を最初に、`context` → `suggestion` の順
   - **コンテキストポインタ**: 次に読むべきファイル一覧
6. ユーザーに現状サマリーを日本語で報告
7. 次のアクションを提案（「何から始めますか？」）
