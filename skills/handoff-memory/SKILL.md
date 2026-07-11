---
name: handoff-memory
description: "Project memory — save, query, merge, and clean up durable project knowledge (lessons, rules, conventions, gotchas). Triggers on 'メモリ保存', 'これ覚えて', '知見を残して', '過去の知見', 'memory save', 'memory query', 'remember this', 'recall', 'cleanup memories', or when recording post-incident learnings, recurring NG patterns, or project-specific conventions."
---

# Handoff Memory Skill

## When to use

- The user says "メモリ保存", "これ覚えて", "知見を残して", "remember this", "save this as a memory"
- The user asks "過去の知見を確認", "recall", "what do we know about X"
- You discover a reusable lesson, recurring NG pattern, or project-specific convention worth persisting across sessions
- Post-incident feedback or investigation results should be captured
- Existing memories need merging, updating, or pruning
- Session start — run cleanup to keep the memory store healthy

## Granularity Rule

**One memory = one piece of knowledge.** Do not bundle multiple unrelated lessons
into a single memory. Each memory should be independently searchable and
independently deletable. If two ideas always travel together, save them together;
if they could be relevant in different contexts, split them.

## Saving a Memory

Call `handoff_memory_save` with:

| Param | Required | Description |
|---|---|---|
| `text` | yes | The memory content. Be specific and actionable — "always use X because Y", not "X is important" |
| `kind` | no (default: `lesson`) | One of: `lesson` (something learned), `rule` (a mandated practice), `convention` (an agreed pattern), `gotcha` (a non-obvious trap) |
| `scope_paths` | no | Path prefixes this memory applies to (e.g. `["src/storage/", "src/mcp/handlers/"]`). Memories with matching scope_paths get a relevance boost when the user edits files under those paths |
| `tags` | no | Free-form tags for searchability (e.g. `["atomic-write", "config"]`). Tags are included in the similarity index |
| `force` | no | `true` to skip near-duplicate detection and save unconditionally |
| `merge_into` | no | ID of an existing memory to overwrite with merged content (see Merging below) |
| `absorb_ids` | no | IDs of memories to delete after merging into the target |

### scope_paths Best Practices

- Use directory-level substrings, not full file paths: `"src/storage/"` not `"src/storage/config.rs"`. The matching is substring-based (`file_path.contains(scope)`), so `"storage/"` will match any file whose path contains that segment
- Scope broadly enough to catch related files, narrowly enough to avoid noise
- A memory with no scope_paths relies purely on text similarity for injection — fine for project-wide rules, but file-specific gotchas benefit from scoping

### tags Best Practices

- Use lowercase, hyphenated terms: `"atomic-write"`, `"json-rpc"`, `"config-toml"`
- Tags supplement the body text for search — add tags for synonyms or concepts not explicitly mentioned in the body

## Handling Near-Duplicate Conflicts

When `handoff_memory_save` returns `status: "conflict"`, it means the new memory
is similar to one or more existing memories (Jaccard similarity >= threshold). The
response includes both the new text and the similar existing memories.

**Decision flow:**

1. **Read both texts carefully.** Are they expressing the same knowledge?
2. **If yes — merge:** Write a combined text that captures both perspectives, then call `handoff_memory_save` again with:
   - `text`: the merged content
   - `merge_into`: ID of the existing memory to keep
   - `absorb_ids`: IDs of any other similar memories to absorb
   - Update `kind`, `tags`, `scope_paths` as needed
3. **If no — force-save:** The memories are genuinely distinct despite textual similarity. Call `handoff_memory_save` with `force: true` to save the new one separately.

Do not ignore conflicts — they indicate the memory store may have redundancy that degrades injection quality.

## Querying Memories

Call `handoff_memory_query` with:

| Param | Description |
|---|---|
| `text` | The prompt or question to match against (BM25 relevance ranking) |
| `file_paths` | Files being worked on — memories scoped to these paths get a boost |
| `session_id` | Current session ID — suppresses re-injection of memories already seen this session |
| `limit` | Max memories to return (default from config, typically 5) |
| `tool_name` | Tool being used (adds context tokens for matching) |
| `mark_injected` | Whether to record injection for dedup (default: true) |

The query engine uses BM25 text similarity plus a scope_path bonus. Passing
`session_id` enables per-session diff injection: a memory already injected this
session (same content hash) is filtered out, but an edited memory (new hash) is
re-injected.

## Cleanup

### When to Run

Run `handoff_memory_cleanup` at the start of each session (the SessionStart hook
does this automatically if configured). Optional arguments:

| Param | Default | Description |
|---|---|---|
| `apply_exact_merges` | `true` | Set `false` to skip auto-merging and only return recommendations |
| `stale_days` | from config (default 60) | Override the staleness threshold in days |

It performs three passes:

1. **Exact duplicates** — silently auto-merged (lossless; oldest entry kept, others absorbed). Skipped when `apply_exact_merges` is `false`
2. **Near-duplicate clusters** — returned as recommendations for AI-driven merge
3. **Stale memories** — memories not referenced for `stale_days`, returned as recommendations

### Acting on Cleanup Recommendations

- **similar_clusters**: Review each cluster. If the memories are truly redundant, merge them with `handoff_memory_save(merge_into=..., absorb_ids=[...])`. If distinct, leave them.
- **stale**: Consider whether the memory is obsolete (delete with `handoff_memory_delete`) or simply rarely triggered (leave it — low hit_count doesn't mean low value).

### Manual Deletion

Call `handoff_memory_delete` with the memory `id` (full ID or unique prefix). Use
this for memories that are confirmed obsolete, wrong, or superseded by a code
change.

## Memory Hooks (Optional Auto-Injection)

Memory tools can be called manually, but for automatic injection without AI
initiative, configure Claude Code hooks:

| Hook | Tool Call | Purpose |
|---|---|---|
| `UserPromptSubmit` | `handoff_memory_query` (prompt text) | Inject memories relevant to each user prompt |
| `PreToolUse` (`Edit\|Write\|MultiEdit`) | `handoff_memory_query` (file path) | Inject memories scoped to the file being edited |
| `SessionStart` | `handoff_memory_cleanup` | Auto-merge exact duplicates, surface recommendations |

Run `handoff-mcp setup` to install these hooks automatically, or
`handoff-mcp setup --uninstall` to remove them. See the README for the full hook
JSON configuration.

## Memory vs Session vs CLAUDE.md

| Layer | Lifespan | Purpose |
|---|---|---|
| Sessions (`.handoff/sessions/`) | Per-conversation | "What was I doing last time?" |
| Memory (`.handoff/memory/`) | Cross-session, long-lived | "What has this project learned?" |
| `CLAUDE.md` | Permanent | Foundational rules, build commands, repo structure |
| `.claude/skills/` | Permanent | Operational procedures triggered by intent |

Rule changes: discover via memory → codify in skills/CLAUDE.md. Memory is the
source of evidence; skills and CLAUDE.md are the source of authority.

## Memory vs Documents

| | Memory | Documents |
|---|---|---|
| Shape | Short, atomic lesson/rule/convention/gotcha | Structured, multi-section Markdown (specs, designs, ADRs, guides) |
| Storage | One flat entry (`.handoff/memory/`) | Split into fragments with a family tree (`.handoff/docs/`) |
| Relationships | None | parent/child (structural) + `related` (semantic: supersedes/references/implements/extends/conflicts) |
| Task linking | Via `scope_paths` boost only | Bidirectional `task_ids` ↔ `task_links` |
| Injection | Always inline (BM25 similarity) | Staged — `full` body for small fragments, `outline` (headings only) for large ones |

**Boundary rule**: if it is a standalone piece of knowledge that fits in a
paragraph or two (< 1 page, no internal sections), save it as a memory. If it
has structure — sections, a hierarchy, cross-references to other documents —
use document management (`handoff_doc_save`; see the `handoff-docs` skill).

There is **no migration path** between the two stores — they are independent.
If a memory grows into something that needs sections and cross-references,
recreate it as a document with `handoff_doc_save` and delete the memory with
`handoff_memory_delete` rather than expecting an automatic conversion.
