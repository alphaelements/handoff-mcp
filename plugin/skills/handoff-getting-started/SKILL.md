---
name: handoff-getting-started
description: "First steps with handoff-mcp: the shortest path from an empty directory to one finished task (install, initialize, set up, check readiness, brainstorm, requirements, implementation). Triggers on '/handoff-getting-started', 'getting started', 'first project', 'how do I start', 'はじめかた', '最初の一歩', 'チュートリアル'."
user-invocable: true
---

# Handoff Getting Started

The shortest path from an empty directory to one finished task: seven steps, always in this order. Walk the user through them one at a time.

Rules for the whole walk:

- Say every step's "What happens" line to the user, in the user's language, as you start that step, and one line on what changed when it is finished. This holds for each step also when you carry out several of them without a pause.
- Decide nothing about how strict the process is. The beginner preset of step 2 has settled that: offer no alternatives, and when a later step shows a setting the preset already made, keep it.
- When you or the user lose track, call `handoff_next_step`. It names the state of the project and the one operation to do next. In a terminal, `handoff-mcp next-step` gives the same answer.
- A step is finished when its "Done when" line holds. Do not start the next step before that.
- Keep the first round small. One requirement and one task are enough to see the whole path once.
- The loop commands of steps 5 to 7 also have a full name that begins with the plugin, for example `/handoff-task-loop:brainstorm-loop`. Use it when Claude Code does not know the short name.

## Step 1: Install

**What happens:** The `handoff-mcp` program and two Claude Code plugins are installed: `handoff-mcp` brings the tools and these skills, and `handoff-task-loop` brings the loop commands of steps 5 to 7.

The user types these. The first line goes into a terminal, the others into Claude Code:

```
npm install -g handoff-mcp-server
/plugin marketplace add alphaelements/handoff-mcp
/plugin install handoff-mcp@handoff-mcp-marketplace
/plugin install handoff-task-loop@handoff-mcp-marketplace
/plugin enable handoff-task-loop@handoff-mcp-marketplace
/reload-plugins
```

`cargo install handoff-mcp` installs the same program from source. Skip what is already there: whoever reads this skill inside Claude Code has the `handoff-mcp` plugin.

Done when: `handoff-mcp --help` prints the command list in a terminal, and `/brainstorm-loop`, `/requirement-loop` and `/session-loop` are available in Claude Code.

## Step 2: `handoff_init`

**What happens:** The project gets a `.handoff/` directory, where its tasks, documents and session history are kept. The beginner preset switches on the guidance and the checks that the following steps rely on.

Call `handoff_init(project_name: "<name>", preset: "beginner")`. Use the directory name as the name unless the user gives another. In a terminal the same is `handoff-mcp init --project-name <name> --preset beginner`.

Done when: the reply lists what the preset applied.

## Step 3: `handoff-mcp setup`

**What happens:** The project gets a `CLAUDE.md` with the sections the agents read (how to build and test, the coding rules, the structure, the documents), and the handoff server is registered in `.mcp.json`. Every field that could not be filled in is marked `TODO(handoff)`.

1. Before you run anything, tell the user that setup also adds two hooks to their own Claude Code settings, which apply to every project: the hooks bring a project's memories and documents into later conversations by themselves. Wait for the user to agree. This is the one place in the walk where something outside the project directory changes.
2. Run `handoff-mcp setup -y` in the project directory. A user who prefers to run it in a terminal types `handoff-mcp setup` and answers its questions.
3. The hooks start working after Claude Code is restarted. The walk does not depend on them, so go on without restarting.

Done when: `CLAUDE.md` and `.mcp.json` exist in the project directory.

## Step 4: `handoff_doctor`

**What happens:** The readiness check reports, item by item, whether the project is ready for agents to work in it, and says what to do about each item that fails. It never runs a command itself: Claude Code runs the project's commands and passes the results back.

1. Call `handoff_doctor`. Every check is `pass`, `fail` or `skip`, and every `fail` comes with a `next_step`. Do what the `next_step` says.
2. In an empty directory two checks fail, because there is nothing to detect yet: `claude_md_todo` (the `TODO(handoff)` fields) and `commands_defined` (no command is known). Settle them with the user in one short exchange:
   - Ask in one sentence what they want to build, and propose the simplest language and test command for it. They only have to say yes or no.
   - Create the smallest thing that can be tested: one source file and one test that passes.
   - Record the test command with `handoff_update_config(updates: {"commands.test": "<test command>"})`.
   - Replace every `TODO(handoff)` in `CLAUDE.md` with the real value, or with `n/a` when the project has no such step. The test command goes into its Build & Test section as well.
3. Call `handoff_doctor` again. It hands out `commands_to_run`. Run each command in the project directory, then call `handoff_doctor(results: [...])` with one entry per command, giving its `id`, `command`, `exit_code` and `duration_ms`.
4. Repeat until `ready` is `true`.

Done when: `ready` is `true` and `cmd_test` is `pass`.

## Step 5: `/brainstorm-loop`

**What happens:** Claude talks with you about what to build and for whom, has the open points researched, and writes the outcome down as a brainstorm report. You only answer yes, no or "more like this".

The user types `/brainstorm-loop <the idea in one sentence>`. The loop asks before it starts its research. A user who answers "I'll leave the rest to you" lets Claude settle the remaining questions and confirms the result once at the end.

Done when: `handoff_next_step` answers `no_requirements`.

## Step 6: `/requirement-loop`

**What happens:** The brainstorm report becomes requirements, each with acceptance criteria that say how to tell it is met. The loop asks for your approval between its phases and ends by creating the tasks to work on.

The user types `/requirement-loop`. The loop finds the brainstorm report by itself, so nothing has to be repeated.

Done when: `handoff_next_step` answers `ready_tasks`.

## Step 7: `/session-loop`

**What happens:** Claude picks the tasks that are ready, shows a plan and, once you approve it, has agents implement and test each task, writing the test first. A task whose done criteria are all met becomes `done`.

The user types `/session-loop`. Before it plans, the loop repeats the readiness check of step 4, so the test command has to pass. It also looks for a smoke command under `[commands]`; a project without one gets a note that the smoke check was skipped, which is not a failure.

A task can stop one status short of `done`, waiting for the user's review, when the agents left a decision to the user. Go through its done criteria with the user and settle the open point; it becomes `done` then.

Done when: `handoff_list_tasks` shows a task with the status `done`.

## After the first task

- `handoff_next_step` names what comes next. While tasks are ready, that is `/session-loop` again.
- The next conversation starts with `handoff_load_context` (the `handoff-load` skill). It brings back where the work stopped, and under the beginner preset its answer carries the same `next_step`.
- A bug is fixed with `/bugfix-loop <the symptom>`.
- Everything else is in the `handoff` skill.
