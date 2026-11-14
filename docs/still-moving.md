# What limits remain?

[← Gate](../README.md)

| Current limit | Reason | What removes it |
| --- | --- | --- |
| Worktrees isolate files, rather than process access | Gate inherits OpenCode's trust model | Use a separate executor sandbox when tasks need process isolation |
| The shell deny list matches prefixes | Gate forwards OpenCode's permission requests; it does not parse shell programs | Enforce sensitive operations in OpenCode's own policy or an executor sandbox |
| Recovery requires the same backend URL | Records identify the service by URL | A stable server identity from OpenCode |
| Jobs owned by another live Gate process stay with that process | One broker must answer each job's permissions | A shared Gate service if clients need live cross-process ownership transfer |
| A crash or lost response during prompt admission can leave the result uncertain | HTTP admission and local persistence are separate operations | An idempotent prompt API; inspect messages and cancel before retrying |
| Worktree setup follows OpenCode's project configuration | OpenCode owns creation and can run configured setup commands | Review that configuration before delegating in an unfamiliar project |
| Empty diffs outside git do not prove that files stayed unchanged | OpenCode records diffs through git snapshots | Use a git worktree or inspect files and messages |
| State and artifacts remain until the user removes them | Automatic cleanup could delete evidence or unreviewed work | Add retention only when a retention policy is required |
| The automated suite uses a local OpenCode fixture | CI has no configured model service | Run the opt-in live test after OpenCode upgrades |

The [runtime reference](tools.md) owns the current job contract.
The [configuration reference](configuration.md) owns permission behavior.
