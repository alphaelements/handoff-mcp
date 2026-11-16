## Build & Test

Commands the agents run. The source of truth is the `[commands]` section of
`.handoff/config.toml`; this section is a copy for people and agents to read.
When a command changes, update `[commands]` and this section together. Replace
each unfilled marker below with the real command, or with `n/a` when the project
has no such step.

- install: {{install}}
- build: {{build}}
- test: {{test}}
- lint: {{lint}}
- format: {{format}}
- typecheck: {{typecheck}}
- smoke: {{smoke}}
- audit: {{audit}}
