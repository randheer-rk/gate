# How do I configure Gate?

[← Gate](../README.md)

Gate reads environment variables at startup. [`GateConfig`](<code:src/config.rs#pub struct GateConfig {>) defines the settings.
[`discover`](<code:src/config.rs#    pub fn discover()>) resolves the environment and OpenCode's registration files.

| Variable | Default | Meaning |
| --- | --- | --- |
| `GATE_OPENCODE_URL` | Registration, then `http://127.0.0.1:4096` | OpenCode HTTP base URL |
| `GATE_OPENCODE_PASSWORD` | Registration | HTTP Basic password for user `opencode` |
| `GATE_DIRECTORY` | `CLAUDE_PROJECT_DIR`, then cwd | Default project directory |
| `GATE_AGENT` | OpenCode's choice | Agent id |
| `GATE_MODEL` | OpenCode's choice | `provider/model` or `provider/model#variant` |
| `GATE_WAIT_MS` | `45000`, capped at `120000` | Default budget for `delegate` and `wait` |
| `GATE_PERMISSION_MODE` | `auto` | `auto`, `ask`, or `guarded` |
| `GATE_DENY_SHELL` | `git push,sudo ,rm -rf /,shutdown,reboot` | Comma-separated denied shell prefixes |
| `GATE_STATE_DIR` | `$XDG_STATE_HOME/gate/jobs` | Job recovery records and ownership locks |
| `GATE_ARTIFACT_DIR` | `$XDG_STATE_HOME/gate/artifacts` | Completed transcripts and patches |

`OPENCODE_URL` and `OPENCODE_PASSWORD` are fallback environment names.
Gate reads `opencode/service.json` from the XDG state directory, then the XDG config directory.
Environment values override registrations. Invalid wait budgets or permission modes fail startup.
The [configuration tests](<code:src/config.rs#    fn wait_config_rejects_invalid_values_and_caps_budgets() {>) pin validation and budget limits.

[`state_home`](<code:src/config.rs#pub fn state_home()>) defaults to `~/.local/state`.
[`config_home`](<code:src/config.rs#pub fn config_home()>) defaults to `~/.config`.
Gate requires the same state directory and backend URL to recover jobs after a restart.

## Permissions

OpenCode decides which actions need approval. Gate answers those requests through
[`Policy::decide`](<code:src/policy.rs#    pub fn decide(&self, request: &PermissionRequest)>).

| Mode | Gate's answer |
| --- | --- |
| `auto` | Approve actions outside the deny list |
| `ask` | Surface each request to the orchestrator |
| `guarded` | Surface `shell` and `bash`; approve other actions outside the deny list |

The deny list takes priority in every mode, including manual approvals through Gate.
Use OpenCode's own permission rules to control actions that it would otherwise allow without a request.
For example, place this file in the project before delegating:

```json
{
  "permission": {
    "edit": "ask",
    "bash": "ask"
  }
}
```

Save it as `opencode.json`. With `GATE_PERMISSION_MODE=ask`, Gate returns
`blocked_on_permission` when OpenCode asks about an edit or shell command.
The [live test](<code:tests/live.py#def run():>) exercises this exchange across a Gate restart.

`reply=once` approves the pending action. `always` creates a durable project rule.
`reject` denies every pending request in that OpenCode session.
Prefer `once` unless the user wants a permanent rule.

## Connection checks

Run `gate doctor --json`. Its report checks the API, agents, session listing, and SSE connection.
It reports failures with a nonzero exit code and hides the configured password.
See [`doctor`](<code:src/main.rs#async fn doctor(json_output: bool)>) and the
[redaction test](<code:src/config.rs#    fn redaction_never_leaks_password() {>).

If a request returns HTTP 401, check the password in the current registration.
If a connection fails, check that OpenCode is running at the reported URL.
Doctor makes read-only requests and does not recover jobs or answer their permissions.
