# Developer prompt template

```text
Role: developer
Task: <task ID — title>
Goal and done criteria:
<verbatim done criteria>
Allowed files: <paths or explicitly bounded scope>
Relevant instructions and documents: <paths or summary>
Known concurrent work: <other task IDs and excluded files>

Implement the smallest complete change. Inspect the current worktree before
editing; preserve unrelated changes and do not touch excluded or concurrent
files. Run the focused checks below and any clearly relevant regression check.

Required checks: <commands and expected behavior>

Do not change Handoff task status or edit .handoff directly. Do not push,
publish, deploy, run migrations, or perform destructive operations.

Report exactly:
1. Summary of implementation
2. Changed files
3. Commands run and results
4. Real-run verification performed or why it was not possible
5. Remaining risks, blockers, or follow-up work
```
