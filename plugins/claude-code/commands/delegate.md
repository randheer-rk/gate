---
description: Delegate a coding task to OpenCode and review the result
argument-hint: <task and acceptance criteria>
---

Use Gate's MCP tools to complete this task: $ARGUMENTS

Plan the work, then call `delegate` with a task brief and the absolute project
directory. Write the brief under the headings in Gate's server instructions: Goal,
Context, Requirements, Edge cases, Out of scope, Tests, Quality, Verify and Report. Use `agents` and `models` when the task needs a specific executor.
Use `worktree: true` for independent changes that should start from committed files.
Call `wait` with budgets below 60 seconds until the job finishes or needs a permission.
Answer permission requests according to the user's instructions. Prefer `once`;
`always` creates a durable OpenCode allow rule.

Review `diff` before reporting success. Use `messages` for details and `steer` for
corrections while the job runs. To continue a finished job, call `delegate` with
its `session_id`. Report the actual checks run and any remaining limits.
