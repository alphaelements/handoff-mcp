# Mermaid templates for spec-loop

Starting points for the generation agents and the coordinator (REQ-SL-010). Copy the shape, replace
the content. Every block here is checked with `mmdc` by the repository tests, so a template that
no longer renders is a bug in this file.

Rules that apply to every diagram:

- The first line is the diagram type (`flowchart TD`, `C4Context`, `stateDiagram-v2`,
  `sequenceDiagram`); no text before it, no frontmatter.
- Node ids are ASCII (`GW`, `S_LOGIN`); human text goes into the label. Never use `end` as an id.
- Quote every label that contains spaces, punctuation or non-ASCII: `GW["ゲートウェイ (MQTT)"]`.
  Non-ASCII (e.g. Japanese) labels work as is - the flowchart template below keeps bilingual
  labels on purpose to demonstrate it; the PDF pipeline switches `htmlLabels` off itself, so do not put
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

  Person(user, "User", "Checks measurement results")
  Person(admin, "Administrator", "Registers devices")

  System_Boundary(b1, "System under design") {
    System(core, "Monitoring platform", "Collects, stores and displays measurements")
  }

  System_Ext(push, "Notification service", "Delivers push notifications")
  System_Ext(idp, "Identity provider", "Sign-in")

  Rel(user, core, "Views", "HTTPS")
  Rel(admin, core, "Registers", "HTTPS")
  Rel(core, push, "Requests notifications", "HTTPS/JSON")
  Rel(core, idp, "Delegates authentication", "OIDC")
```

## Phase 2 - stateDiagram-v2 (screen transitions)

Screens are states, user actions label the edges. Declare each state with an ASCII id and a
quoted description so the label shows the screen name. Include `[*]` start and end, and the error
and empty states the mocks show. Use a `note` for a guard condition that does not fit an edge label.

```mermaid
stateDiagram-v2
  [*] --> S_LOGIN

  state "Login screen (S_LOGIN)" as S_LOGIN
  state "Home screen (S_HOME)" as S_HOME
  state "Measurement detail screen (S_DETAIL)" as S_DETAIL
  state "Settings screen (S_SETTINGS)" as S_SETTINGS
  state "Error screen (S_ERROR)" as S_ERROR

  S_LOGIN --> S_HOME: Sign-in succeeded
  S_LOGIN --> S_ERROR: Authentication failed
  S_ERROR --> S_LOGIN: Retry button
  S_HOME --> S_DETAIL: Tap a measurement card
  S_DETAIL --> S_HOME: Back
  S_HOME --> S_SETTINGS: Gear icon
  S_SETTINGS --> S_HOME: Save and return
  S_HOME --> [*]: Sign out

  note right of S_ERROR
    Locked after 3 consecutive failures
  end note
```

## Phase 3 - sequenceDiagram (reference for the use-case analyst)

Participants are the architecture components, with the same names. One diagram per use case that
crosses several components; label messages with the data, use `alt` / `opt` for the alternative
scenarios.

```mermaid
sequenceDiagram
  actor U as User
  participant UI as Mobile app
  participant SVC as Aggregation service
  participant DB as Time-series DB

  U->>UI: Pick a period and open it
  UI->>SVC: Request aggregation (period)
  SVC->>DB: Fetch readings
  DB-->>SVC: Readings
  alt Data available
    SVC-->>UI: Aggregated result
    UI-->>U: Show the chart
  else No data
    SVC-->>UI: Empty result
    UI-->>U: Show "No data available"
  end
```

## Validation

```bash
mmdc -i diagram.mmd -o diagram.svg      # binary from HANDOFF_MMDC_BIN when set
```

Write the temp files outside the project tree. Non-zero exit = syntax error: fix and re-run before
the diagram is shown to the user. Without `mmdc` the diagram is `unverified` and the specification
says "diagrams not validated".
