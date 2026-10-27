# Mermaid templates for spec-loop

Starting points for the generation agents and the coordinator (REQ-SL-010). Copy the shape, replace
the content. Every block here is checked with `mmdc` by the repository tests, so a template that
no longer renders is a bug in this file.

Rules that apply to every diagram:

- The first line is the diagram type (`flowchart TD`, `C4Context`, `stateDiagram-v2`,
  `sequenceDiagram`); no text before it, no frontmatter.
- Node ids are ASCII (`GW`, `S_LOGIN`); human text goes into the label. Never use `end` as an id.
- Quote every label that contains spaces, punctuation or non-ASCII: `GW["ゲートウェイ (MQTT)"]`.
  Japanese labels work as is; the PDF pipeline switches `htmlLabels` off itself, so do not put
  `%%{init}%%` directives or HTML (`<br/>`) in the diagram.
- Keep one diagram under about 15 nodes. Split by subsystem instead of cramming; a second diagram
  per subsystem is better than an unreadable first one.
- Name components exactly as in the architecture diagram wherever they reappear (Phase 3
  `sequenceDiagram` participants, Phase 2 notes).
- Trace comments: `%% BS-SCOPE-001` after a node is allowed and helps the review; the tables next
  to the diagram remain the source of the traceability.

## Phase 1 - flowchart TD (default: structure + data flow)

Conventions: `subgraph` = subsystem or trust boundary; `([...])` stadium = external actor or system;
`[(...)]` cylinder = data store; `[...]` rectangle = internal component; labelled edge = data
flowing in the arrow's direction; dashed edge `-.->` = asynchronous or optional flow.

```mermaid
flowchart TD
  USER([利用者 / User])
  EXT_API([外部 API / Third-party API])

  subgraph DEVICE["デバイス / Device"]
    SENSOR["センサー管理 / Sensor manager"]
    BUF["送信バッファ / Upload buffer"]
  end

  subgraph BACKEND["バックエンド / Backend"]
    GW["ゲートウェイ / Gateway"]
    SVC["集計サービス / Aggregation service"]
    DB[("時系列DB / Time-series DB")]
  end

  subgraph APP["モバイルアプリ / Mobile app"]
    UI["画面 / UI"]
  end

  SENSOR -->|測定値 / readings| BUF
  BUF -->|MQTT バッチ / batch| GW
  GW -->|検証済みデータ / validated data| SVC
  SVC -->|保存 / write| DB
  UI -->|照会 / query| SVC
  SVC -.->|通知 / notification| EXT_API
  USER -->|操作 / interaction| UI
```

## Phase 1 - C4Context (people and external systems dominate)

Use when most of the story is "who talks to which external system". Add a `flowchart TD` (above)
for the internals. Keep to `Person`, `System`, `System_Ext`, `System_Boundary`, `Rel`; fancier
C4 elements render inconsistently across mmdc versions.

```mermaid
C4Context
  title システムコンテキスト / System context

  Person(user, "利用者", "測定結果を確認する")
  Person(admin, "管理者", "デバイスを登録する")

  System_Boundary(b1, "対象システム") {
    System(core, "モニタリング基盤", "測定値の収集・保存・表示")
  }

  System_Ext(push, "通知サービス", "プッシュ通知の配信")
  System_Ext(idp, "認証基盤", "ログイン")

  Rel(user, core, "閲覧する", "HTTPS")
  Rel(admin, core, "登録する", "HTTPS")
  Rel(core, push, "通知を依頼する", "HTTPS/JSON")
  Rel(core, idp, "認証を委譲する", "OIDC")
```

## Phase 2 - stateDiagram-v2 (screen transitions)

Screens are states, user actions label the edges. Declare each state with an ASCII id and a
quoted description so the label shows the screen name. Include `[*]` start and end, and the error
and empty states the mocks show. Use a `note` for a guard condition that does not fit an edge label.

```mermaid
stateDiagram-v2
  [*] --> S_LOGIN

  state "ログイン画面 (S_LOGIN)" as S_LOGIN
  state "ホーム画面 (S_HOME)" as S_HOME
  state "測定詳細画面 (S_DETAIL)" as S_DETAIL
  state "設定画面 (S_SETTINGS)" as S_SETTINGS
  state "エラー画面 (S_ERROR)" as S_ERROR

  S_LOGIN --> S_HOME: ログイン成功
  S_LOGIN --> S_ERROR: 認証失敗
  S_ERROR --> S_LOGIN: 再試行ボタン
  S_HOME --> S_DETAIL: 測定カードをタップ
  S_DETAIL --> S_HOME: 戻る
  S_HOME --> S_SETTINGS: 歯車アイコン
  S_SETTINGS --> S_HOME: 保存して戻る
  S_HOME --> [*]: ログアウト

  note right of S_ERROR
    3回連続で失敗するとロックされる
  end note
```

## Phase 3 - sequenceDiagram (reference for the use-case analyst)

Participants are the architecture components, with the same names. One diagram per use case that
crosses several components; label messages with the data, use `alt` / `opt` for the alternative
scenarios.

```mermaid
sequenceDiagram
  actor U as 利用者
  participant UI as モバイルアプリ
  participant SVC as 集計サービス
  participant DB as 時系列DB

  U->>UI: 期間を選んで開く
  UI->>SVC: 集計を要求 (期間)
  SVC->>DB: 測定値を取得
  DB-->>SVC: 測定値
  alt データあり
    SVC-->>UI: 集計結果
    UI-->>U: グラフを表示
  else データなし
    SVC-->>UI: 空の結果
    UI-->>U: 「データがありません」を表示
  end
```

## Validation

```bash
mmdc -i diagram.mmd -o diagram.svg      # binary from HANDOFF_MMDC_BIN when set
```

Write the temp files outside the project tree. Non-zero exit = syntax error: fix and re-run before
the diagram is shown to the user. Without `mmdc` the diagram is `unverified` and the specification
says "diagrams not validated".
