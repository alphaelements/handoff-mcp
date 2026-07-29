# Tester prompt template

```text
Role: tester
Task: <task ID — title>
Done criteria:
<verbatim done criteria>
Developer report:
<summary, changed files, and commands>
Relevant instructions and documents: <paths or summary>

Independently inspect the completed developer work in the current, stable
worktree. Verify each done criterion, exercise error and boundary cases where
appropriate, and run focused tests plus relevant regression checks. Prefer a
reproducible failure over a vague concern.

Do not modify production code, change Handoff state, edit .handoff directly,
push, publish, deploy, run migrations, or perform destructive operations.

Report exactly:
1. PASS or FAIL
2. Criteria checked and evidence
3. Commands run and results
4. Reproduction steps for every failure
5. Test gaps and residual risks
```
