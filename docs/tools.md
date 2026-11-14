# How do I delegate and review a task?

[← Gate](../README.md)

1. Call `delegate` with a self-contained task, a directory, and acceptance criteria.
2. If the result is `running`, call `wait` with a budget below 60 seconds.
3. If it is `blocked_on_permission`, call `reply_permission`.
4. Review `diff`. Read `messages` when the summary lacks a detail.
5. Correct a running job with `steer`. Continue a finished session through `delegate` and `session_id`.

[`GateServer`](<code:src/mcp/server.rs#pub struct GateServer {>) exposes these tools over stdio:

| Tool | Inputs | Result and schema |
| --- | --- | --- |
| `delegate` | Required `task`; optional `directory`, `agent`, `model`, `title`, `files`, `wait_ms`, `worktree`, `session_id` | New job snapshot; [`DelegateArgs`](<code:src/mcp/server.rs#struct DelegateArgs {>) |
| `wait` | `job_id` or `session_id`; optional `wait_ms`, `stop_on_permission` | Snapshot after completion, a permission request, or budget expiry; [`WaitArgs`](<code:src/mcp/server.rs#struct WaitArgs {>) |
| `status` | `job_id` or `session_id` | Current snapshot; [`JobRefArgs`](<code:src/mcp/server.rs#struct JobRefArgs {>) |
| `messages` | Job/session; optional `limit`, `include_tool_calls`, `include_system`, `max_chars` | Recent messages in chronological order; [`MessagesArgs`](<code:src/mcp/server.rs#struct MessagesArgs {>) |
| `steer` | Job/session, `text`; optional `delivery=steer` or `queue` | Snapshot after prompt admission; [`SteerArgs`](<code:src/mcp/server.rs#struct SteerArgs {>) |
| `cancel` | Job/session | Cancelled or already finished snapshot; files remain |
| `diff` | Job/session; optional `include_patch`, `max_patch_chars`, `context` | File totals and bounded patches; [`DiffArgs`](<code:src/mcp/server.rs#struct DiffArgs {>) |
| `reply_permission` | Job/session, `reply=once`, `always`, or `reject`; optional `request_id`, `message` | Snapshot after the answer; [`ReplyPermissionArgs`](<code:src/mcp/server.rs#struct ReplyPermissionArgs {>) |
| `list` | None | This process's jobs and the jobs it recovered, newest first |
| `agents` | Optional `directory`, `include_hidden` | Agent ids and descriptions; [`AgentsArgs`](<code:src/mcp/server.rs#struct AgentsArgs {>) |
| `models` | Optional `directory` | Enabled model names and variants; [`ModelsArgs`](<code:src/mcp/server.rs#struct ModelsArgs {>) |

`files` contains objects with `uri` and optional `name` and `description`.
Relative directories resolve against Gate's default directory and must exist.
The [directory tests](<code:src/jobs/task.rs#    fn resolves_relative_to_default() {>) pin this behavior.

## Snapshots and artifacts

[`JobSnapshot`](<code:src/jobs/registry.rs#pub struct JobSnapshot {>) includes the job and session ids,
status, directory, actual agent and model, summary, progress, permissions, usage, diff totals, and artifact path.
`next` tells the orchestrator which call can advance the job.
`git` reports whether the directory is a git worktree.

Jobs have five states: `running`, `blocked_on_permission`, `completed`, `failed`, and `cancelled`.
The [waiter tests](<code:src/jobs/registry.rs#    async fn wait_stops_on_permission_when_asked() {>) cover permission wakeups.
Completion fixes `elapsed_ms`; later status calls do not increase it.

Gate clips summaries, text tails, and patches in tool responses.
The private artifact contains the job's complete message range and patches.
[`finish_locked`](<code:src/jobs/dispatcher.rs#    async fn finish_locked(>) retrieves every transcript page before writing it.
It reports retrieval or storage failures in the snapshot's `error` field.

## Worktrees and sessions

With `worktree=true`, Gate asks OpenCode to create a worktree from the selected checkout's committed files.
Uncommitted and untracked source files do not carry over.
Gate returns the new `directory`, source directory, project id, and branch when one exists.
OpenCode can create a detached HEAD; `worktree.branch` is then `null`.
The worktree remains after completion, cancellation, or a Gate restart.
Review it at the returned path before committing or merging changes.

Parallel tasks use separate sessions. Use separate worktrees when their edits must be isolated.
`worktree=true` and `session_id` cannot be combined.
The [protocol test](<code:tests/smoke.py#def run():>) checks these paths with disposable git repositories.

To continue a finished session, pass its `session_id` to `delegate`.
Gate creates a new job and pins its diff to that job's first and last user messages.
Earlier jobs keep their own diff ranges and transcript artifacts.
An active session accepts `steer` or `queue`, rather than another `delegate` call.

Another client can continue the same session through its own Gate process, for example a reviewer sending fixes.
Each snapshot names the MCP `client` that delegated it, such as `claude-code` or `codex-mcp-client`.
Called with `session_id`, `diff` runs to the session's latest prompt when another client continued it after this process's job.
`cancel` with `session_id` also interrupts a turn that another client started on the session.
The [fixture suite](<code:tests/smoke.py#def run():>) covers both cases.

## Restart recovery

[`Registry::open`](<code:src/jobs/registry.rs#    pub fn open(directory: PathBuf, backend: String)>) loads records for the current backend URL.
Operating system locks prevent Gate from claiming jobs owned by another live Gate process.
Each process lists its own jobs and records it recovered at startup.
An untracked raw `session_id` still works with `messages`, `diff`, and an explicit permission request id.

[`reconcile`](<code:src/jobs/dispatcher.rs#    pub async fn reconcile(>) checks OpenCode after missed events and at regular intervals.
A pending permission takes priority over an inactive execution drain.
Gate waits for an explicit outcome before declaring success.
It does not resend a prompt after a crash with uncertain admission.
The [recovery test](<code:src/jobs/registry.rs#    async fn recovery_keeps_state_and_only_claims_unowned_jobs() {>) covers record ownership and stable completed state.
