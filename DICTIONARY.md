# Gate terms

[← Gate](README.md)

- **Artifact:** A private JSON file that holds a finished job's messages, diff, and result.
- **Backend:** The OpenCode HTTP service to which Gate sends tasks.
- **Gate:** The MCP server in this repository.
- **Job:** Gate's record of one delegated task, including its OpenCode session and message range.
- **MCP:** Model Context Protocol, used by a client to discover and call Gate's tools.
- **Orchestrator:** The client that plans work and reviews OpenCode's changes.
- **Permission request:** An OpenCode request that blocks execution until a client answers it.
- **Session:** OpenCode's durable conversation; several jobs can use it in sequence.
- **Snapshot:** The bounded job state returned by a Gate tool.
- **SSE:** Server-Sent Events, the HTTP stream that carries OpenCode's progress.
- **Worktree:** A separate git checkout where OpenCode can edit files without changing the caller's checkout.
