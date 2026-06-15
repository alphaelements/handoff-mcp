---
name: handoff-import
description: "Import existing handoff documents into .handoff/ management. Reads the specified file, structures its content into tasks/decisions/blockers/notes, and calls handoff_import_context in one shot. Triggers on '/handoff-import <path>', '引継ぎ資料をインポート', 'この資料をhandoffに取り込んで'."
user-invocable: true
---

# Handoff Import

既存の引継ぎ資料（Markdown、テキスト、JSON など）を読み取り、
handoff MCP の管理下（`.handoff/`）に一括移行する。

## 使い方

```
/handoff-import tmp/260601-sprint-handoff.md
/handoff-import path/to/any-document.md
```

引数なしで呼ばれた場合は、ユーザーにファイルパスを尋ねる。

## 手順

1. **初期化チェック**: `.handoff/` が存在するか確認。なければ `handoff_init` で初期化。

2. **資料の読み取り**: 指定されたファイルを `Read` で読む。

3. **内容の分析と構造化**: 資料の内容を以下のカテゴリに分解する:

   ### タスク (`tasks`)
   - 作業項目、TODO、チケット、課題をタスクとして抽出
   - 親子関係が読み取れる場合は `children` でネスト
   - ステータスを推定: 「完了」→ `done`、「作業中」→ `in_progress`、「未着手」→ `todo`、「保留」→ `blocked`
   - 優先度の手がかり（P0/P1、「最優先」等）があれば `priority` を設定
   - 完了条件が書かれていれば `done_criteria` に変換
   - 工数見積もりがあれば `schedule.estimate_hours` に設定
   - 期限・マイルストーンがあれば `schedule.due_date` / `schedule.milestone` に設定
   - 依存関係（「〜の後に」「〜が完了したら」）があれば `dependencies` に設定
   - 表示順の手がかり（P0→P1→P2 等）があれば `order` に設定

   ### セッション情報 (`session`)
   - **summary**: 資料の概要を1行で（`[import]` プレフィックス付き）
   - **decisions**: 意思決定・方針・採用技術を抽出。confidence は:
     - 明確に決定済み → `confirmed`
     - 方針レベル → `estimated`
     - 検討中・未確定 → `unverified`
   - **blockers**: 未解決の課題、待ち事項
   - **handoff_notes**: 注意事項 (`caution`)、背景情報 (`context`)、提案 (`suggestion`)
   - **references**: 資料中のリンク、関連ドキュメント、issue 番号
   - **context_pointers**: 言及されているソースファイルのパス

   ### 構造化できない情報 (`raw_notes`)
   - 上記カテゴリに分類しきれない情報はここに入れる
   - 空にせず、拾いきれなかった情報は積極的に入れる

4. **インポート実行**: `handoff_import_context` を1回呼んで一括投入。

5. **結果の報告**: 作成されたタスク数、セッション保存有無、raw_notes の有無を報告。

6. **確認**: `handoff_list_tasks` でインポート結果を表示し、
   ユーザーに内容が正しいか確認を求める。

## 構造化のガイドライン

- **迷ったらタスクにする**: 作業項目かどうか微妙なものはタスクとして登録し、
  notes に元の文脈を残す。後から skipped にできる。
- **情報を捨てない**: 元資料の情報は必ずどこかに入れる。
  タスク・セッション・raw_notes のいずれかに必ず含める。
- **元資料への参照**: `references` に元ファイルのパスを必ず入れる。
- **推定は明示する**: ステータスや優先度を推定した場合は notes に「（推定）」と記載。

## source フィールドの設定

```json
{
  "description": "<ファイルパス> からのインポート",
  "format": "markdown"  // or "json", "text", "other"
}
```

ファイル拡張子から format を判定:
- `.md` → `markdown`
- `.json` → `json`
- `.txt` → `text`
- その他 → `other`
