//! MCP tool router and handlers.

use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Gateway;
use crate::jobs::registry::{DiffStat, Job, JobSnapshot, JobStatus};
use crate::jobs::task::{self, MAX_WAIT_MS, StartRequest};
use crate::opencode::types::{Delivery, FileAttachment, PermissionReply};

const INSTRUCTIONS: &str = "\
Gate delegates coding work to a local OpenCode agent running in the caller's project directory.

Workflow:
1. `delegate` with a task brief (see below). It returns a job_id and a snapshot.
2. If the snapshot says `running`, call `wait` with short budgets and repeat. If it says `blocked_on_permission`, call `reply_permission`.
3. Review the result with `diff` (patches) and, when the summary is not enough, `messages`.
4. Correct a live job with `steer`, stop it with `cancel`. `status` peeks without blocking; `list` shows every job in this process.

Things to know:
- Gate edits the selected directory. Pass worktree=true to isolate a new task in an OpenCode-managed git worktree; its path and branch are returned and kept for review.
- Always read `diff` before reporting success: the summary describes intent, the patch is the fact.
- OpenCode builds diffs from git snapshots. Snapshots carry a `git` flag; when it is false, `diff` will be empty even if files changed, so read the files instead.
- Keep `wait` under 60 seconds per call; some MCP clients time out longer tool calls.
- Permission behaviour is set by GATE_PERMISSION_MODE (auto, ask, guarded), not per call.
- Returned values are compact JSON. Full transcripts and patches also land in the artifact file named in the snapshot.

Writing the task brief:
The agent sees only the task text and the repository, never this conversation. Use these headings, and drop one only when it truly does not apply:
1. Goal: the outcome and why it matters, in one to three sentences.
2. Context: files to read first, an existing file whose pattern to copy, related code, and decisions already made. For a bug: exact reproduction steps, expected versus actual result, and the error output.
3. Requirements: numbered and testable, with concrete values: \"WHEN <trigger>, <component> SHALL <observable result>\". UI work also lists every state (loading, empty, error, success, disabled), the layout at mobile (375px), tablet (768px) and desktop (1280px) widths, keyboard and screen-reader behaviour, the components and design tokens to reuse, and exact copy text.
4. Edge cases: boundaries, empty and invalid input, failures and timeouts, permissions, concurrency, and Unicode, each with its expected result.
5. Out of scope: files and behaviour to leave alone. No new dependencies unless this brief names them.
6. Tests: what to add at each level that applies. Unit tests cover logic, one per requirement and edge case. Integration tests cover real boundaries such as the database, HTTP, the filesystem or another module. Smoke or end-to-end tests drive the main path through the real entry point. A bug fix starts with a regression test that fails before the fix. Name the test framework and files.
7. Quality: follow AGENTS.md and the code around the change; mirror the named file for structure, naming and error handling; make the smallest diff that meets the requirements; leave no dead code, TODOs or unrelated refactors.
8. Verify: the exact commands that must pass, such as format check, lint, typecheck, tests and build. For UI, also run the app and check each state at each width.
9. Report: files changed, each verify command with its result, assumptions or deviations, and open questions. Say whether to commit.";

pub struct GateServer {
    gateway: Arc<Gateway>,
}

impl GateServer {
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DelegateArgs {
    /// Task brief with the headings Goal, Context, Requirements, Edge cases,
    /// Out of scope, Tests, Quality, Verify and Report (the server
    /// instructions describe each). The delegated agent cannot see this
    /// conversation, so do not refer to "the above".
    task: String,
    /// Absolute path, or a path relative to Gate's default directory. Defaults
    /// to the directory Gate was started in (the client's project root).
    #[serde(default)]
    directory: Option<String>,
    /// OpenCode agent id from `agents`, e.g. `build`, `plan`, `explore`.
    #[serde(default)]
    agent: Option<String>,
    /// Model as `provider/model` or `provider/model#variant`.
    #[serde(default)]
    model: Option<String>,
    /// Session title. Defaults to the first line of `task`.
    #[serde(default)]
    title: Option<String>,
    /// Files to attach to the first prompt, each `{uri, name?, description?}`.
    #[serde(default)]
    files: Vec<FileArg>,
    /// How long to block before returning the job id. Defaults to
    /// GATE_WAIT_MS (45s), clipped to 120s.
    #[serde(default)]
    wait_ms: Option<u64>,
    /// Create a git worktree through OpenCode, starting from committed files.
    /// The worktree and branch remain on disk for review after the job finishes.
    #[serde(default)]
    worktree: Option<bool>,
    /// Continue an idle OpenCode session. Active sessions accept steer/queue instead.
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileArg {
    /// `file://` URI or absolute path of the file to attach.
    uri: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct JobRefArgs {
    /// Job id returned by `delegate`.
    #[serde(default)]
    job_id: Option<String>,
    /// OpenCode session id, as an alternative to `job_id`.
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WaitArgs {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// Maximum time to block. Defaults to GATE_WAIT_MS (45s), clipped to 120s.
    #[serde(default)]
    wait_ms: Option<u64>,
    /// Return early when the job needs a permission answer. Defaults to true.
    #[serde(default)]
    stop_on_permission: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MessagesArgs {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// Number of newest meaningful messages to return. Defaults to 12, clipped to 100.
    #[serde(default)]
    limit: Option<u32>,
    /// Include one line per tool call. Defaults to true.
    #[serde(default)]
    include_tool_calls: Option<bool>,
    /// Include system/synthetic context messages. Defaults to false; they are
    /// long and rarely useful.
    #[serde(default)]
    include_system: Option<bool>,
    /// Approximate character budget for the whole answer. Defaults to 12000.
    #[serde(default)]
    max_chars: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SteerArgs {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// The correction or additional instruction.
    text: String,
    /// `steer` (default) injects into the current turn; `queue` delivers after it.
    #[serde(default)]
    delivery: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DiffArgs {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// Include patch text. Defaults to true.
    #[serde(default)]
    include_patch: Option<bool>,
    /// Per-file patch limit in characters. Defaults to 6000.
    #[serde(default)]
    max_patch_chars: Option<u64>,
    /// Unchanged context lines around each hunk. Omit for full-file patches.
    #[serde(default)]
    context: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReplyPermissionArgs {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// Permission request id. Defaults to the job's pending request.
    #[serde(default)]
    request_id: Option<String>,
    /// `once`, `always` or `reject`.
    reply: String,
    /// Reason shown to the agent; most useful with `reject`.
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AgentsArgs {
    /// Directory to list agents for. Defaults to Gate's working directory.
    #[serde(default)]
    directory: Option<String>,
    /// Include agents OpenCode normally hides. Defaults to false.
    #[serde(default)]
    include_hidden: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ModelsArgs {
    #[serde(default)]
    directory: Option<String>,
}

#[tool_router]
impl GateServer {
    #[tool(
        name = "delegate",
        description = "Delegate a coding task to a local OpenCode agent. Creates a durable session in `directory`, sends the task and blocks for a short budget. Returns a job_id plus a snapshot: status=running means call `wait`; status=blocked_on_permission means call `reply_permission`; status=completed means review with `diff`."
    )]
    async fn delegate(
        &self,
        peer: rmcp::Peer<rmcp::RoleServer>,
        Parameters(args): Parameters<DelegateArgs>,
    ) -> Result<String, String> {
        let wait = Duration::from_millis(
            args.wait_ms
                .unwrap_or(self.gateway.config.default_wait_ms)
                .min(MAX_WAIT_MS),
        );

        let snapshot = task::start(
            &self.gateway.client,
            &self.gateway.registry,
            &self.gateway.config,
            &self.gateway.dispatcher,
            StartRequest {
                task: args.task,
                directory: args.directory,
                agent: args.agent,
                model: args.model,
                title: args.title,
                files: args
                    .files
                    .into_iter()
                    .map(|file| FileAttachment {
                        uri: file.uri,
                        name: file.name,
                        description: file.description,
                    })
                    .collect(),
                wait,
                worktree: args.worktree.unwrap_or(false),
                session_id: args.session_id,
                client: peer.peer_info().map(|info| info.client_info.name.clone()),
            },
        )
        .await?;

        let mut view = job_view(&snapshot);
        view["waited_ms"] = json!(wait.as_millis() as u64);
        pretty(&view)
    }

    #[tool(
        name = "status",
        description = "Show one delegated job without blocking: status, recent progress lines, streamed partial output, pending permission, token usage, diff stat and artifact path."
    )]
    async fn status(&self, Parameters(args): Parameters<JobRefArgs>) -> Result<String, String> {
        let job = self
            .require_job(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;
        pretty(&job_view(&job.snapshot().await))
    }

    #[tool(
        name = "wait",
        description = "Block until a job finishes, blocks on a permission, or the budget expires. Returns the same snapshot as `delegate`. Use short budgets (under 60s) and call again; MCP clients may time out long calls."
    )]
    async fn wait(&self, Parameters(args): Parameters<WaitArgs>) -> Result<String, String> {
        let job = self
            .require_job(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;
        let budget = Duration::from_millis(
            args.wait_ms
                .unwrap_or(self.gateway.config.default_wait_ms)
                .min(MAX_WAIT_MS),
        );
        let snapshot = job
            .wait(budget, args.stop_on_permission.unwrap_or(true))
            .await;
        let mut view = job_view(&snapshot);
        view["waited_ms"] = json!(budget.as_millis() as u64);
        pretty(&view)
    }

    #[tool(
        name = "messages",
        description = "Read the tail of a job's conversation: assistant and user text plus tool calls, newest content first under a character budget. Use when the summary omits a detail you need."
    )]
    async fn messages(&self, Parameters(args): Parameters<MessagesArgs>) -> Result<String, String> {
        let job = self
            .maybe_job(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;
        let session_id = self
            .session_of(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;

        let limit = args.limit.unwrap_or(12).clamp(1, 100);
        let include_tools = args.include_tool_calls.unwrap_or(true);
        let include_system = args.include_system.unwrap_or(false);
        let budget = args.max_chars.unwrap_or(12_000).clamp(1_000, 50_000) as usize;

        // Over-fetch: system/no-op messages are filtered out below, and a page of
        // 12 raw messages can contain no assistant text at all.
        let fetch = (limit.saturating_mul(3)).clamp(limit, 300);
        let archived = match &job {
            Some(job) if job.status().await.is_terminal() => job.snapshot().await.artifact,
            _ => None,
        };
        let page = if let Some(artifact) = archived {
            let bytes = std::fs::read(&artifact)
                .map_err(|error| format!("could not read artifact {artifact}: {error}"))?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            let mut messages: Vec<crate::opencode::types::Message> =
                serde_json::from_value(value["messages"].clone())
                    .map_err(|error| error.to_string())?;
            messages.reverse();
            messages
        } else {
            self.gateway
                .client
                .message_page(&session_id, Some(fetch), Some("desc"))
                .await
                .map_err(|error| error.to_string())?
        };

        // Page arrives newest first; take as much recent history as fits.
        let mut views = Vec::new();
        let mut used = 0usize;
        for message in &page {
            let keep = match message.kind.as_str() {
                "user" | "assistant" | "shell" => true,
                _ => include_system,
            };
            if !keep {
                continue;
            }

            let remaining = budget.saturating_sub(used);
            if remaining < 32 {
                break;
            }
            let text = truncate(&message.text(), remaining.min(4_000));
            let mut cost = text.chars().count();
            let mut tools = Vec::new();
            if include_tools {
                for tool in message.tool_calls() {
                    if cost + 32 >= remaining {
                        break;
                    }
                    let tool = truncate(&tool, (remaining - cost - 16).min(200));
                    cost += tool.chars().count();
                    tools.push(tool);
                }
            }
            if text.trim().is_empty() && tools.is_empty() {
                continue;
            }

            used += cost;

            let mut view = json!({
                "id": message.id,
                "type": message.kind,
                "text": text,
            });
            if !tools.is_empty() {
                view["tool_calls"] = json!(tools);
            }
            if let Some(finish) = &message.finish {
                view["finish"] = json!(finish);
            }
            if let Some(error) = &message.error {
                view["error"] = json!(truncate(&error.to_string(), 800));
            }
            views.push(view);
            if views.len() >= limit as usize {
                break;
            }
        }
        views.reverse();

        pretty(&json!({
            "session_id": session_id,
            "returned": views.len(),
            "scanned": page.len(),
            "messages": views,
        }))
    }

    #[tool(
        name = "steer",
        description = "Send more input to a running job. delivery=steer (default) injects into the current turn; delivery=queue delivers after it finishes. Use for corrections, extra constraints or a request to fix failing tests."
    )]
    async fn steer(&self, Parameters(args): Parameters<SteerArgs>) -> Result<String, String> {
        let job = self
            .require_job(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;
        let status = job.status().await;
        if status.is_terminal() {
            return Err(format!(
                "job {} already finished with status {}; delegate a new task instead of steering it",
                job.id, status
            ));
        }
        let delivery = parse_delivery(args.delivery.as_deref())?;
        task::steer(&self.gateway.client, &job, &args.text, delivery).await?;
        let mut view = job_view(&job.snapshot().await);
        view["steered_with"] = json!(match delivery {
            Delivery::Steer => "steer",
            Delivery::Queue => "queue",
        });
        pretty(&view)
    }

    #[tool(
        name = "cancel",
        description = "Interrupt a job's current turn and mark it cancelled. Changes already written to disk are kept and can be reviewed with `diff`."
    )]
    async fn cancel(&self, Parameters(args): Parameters<JobRefArgs>) -> Result<String, String> {
        let job = self
            .require_job(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;
        // Our job may be finished while another client (e.g. a reviewer's Gate)
        // runs a later turn on the same session; stopping the agent stops that too.
        if job.status().await.is_terminal() {
            let client = &self.gateway.client;
            let active = client.active_sessions().await.map_err(|e| e.to_string())?;
            if active.contains(&job.session_id) {
                client
                    .interrupt_with_resume(&job.session_id, false)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut view = job_view(&job.snapshot().await);
                view["interrupted"] = json!("a turn another client started on this session");
                return pretty(&view);
            }
        }
        task::cancel(&self.gateway.client, &job).await?;
        self.gateway
            .dispatcher
            .finalize(&job, JobStatus::Cancelled, None)
            .await;
        pretty(&job_view(&job.snapshot().await))
    }

    #[tool(
        name = "diff",
        description = "Show the working-tree changes a job produced: per-file status, additions, deletions and patches. Read this before reporting success; patches may be truncated by the output budget, in which case the job artifact has the full text."
    )]
    async fn diff(&self, Parameters(args): Parameters<DiffArgs>) -> Result<String, String> {
        let job = self
            .maybe_job(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;
        let session_id = match &job {
            Some(job) => job.session_id.clone(),
            None => {
                self.session_of(args.job_id.as_deref(), args.session_id.as_deref())
                    .await?
            }
        };

        let range = match &job {
            Some(job) => Some(job.snapshot().await),
            None => None,
        };
        let mut to = range.as_ref().and_then(|job| job.last_message_id.clone());
        // Asked by session, the caller wants the session's changes. If another
        // client (e.g. a reviewer's Gate) continued it after our job, include them.
        if args.job_id.is_none()
            && range.as_ref().is_some_and(|job| job.status.is_terminal())
            && let Ok(Some(latest)) = self.gateway.client.latest_user_message(&session_id).await
            && Some(&latest.id) != to.as_ref()
        {
            to = None;
        }
        let files = self
            .gateway
            .client
            .diff_range(
                &session_id,
                args.context,
                range
                    .as_ref()
                    .and_then(|job| job.first_message_id.as_deref()),
                to.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())?;

        let include_patch = args.include_patch.unwrap_or(true);
        let max_patch = args.max_patch_chars.unwrap_or(6_000).clamp(200, 100_000) as usize;
        let mut budget = 60_000usize;
        let mut views = Vec::with_capacity(files.len());

        for file in &files {
            let mut view = json!({
                "file": file.file,
                "status": file.status,
                "additions": file.additions,
                "deletions": file.deletions,
            });
            if include_patch {
                let patch = truncate(&file.patch, max_patch);
                if patch.len() <= budget {
                    budget -= patch.len();
                    if patch.len() < file.patch.len() {
                        view["patch_truncated"] = json!(true);
                    }
                    view["patch"] = json!(patch);
                } else {
                    view["patch_omitted"] =
                        json!("output budget reached; the job artifact contains the full patch");
                }
            }
            views.push(view);
        }

        let stat = DiffStat::from_files(&files);
        let artifact = match &job {
            Some(job) => job.snapshot().await.artifact,
            None => None,
        };
        let directory = job.as_ref().map(|job| job.directory.clone());

        let mut result = json!({
            "session_id": session_id,
            "totals": {
                "files": stat.files,
                "additions": stat.additions,
                "deletions": stat.deletions,
            },
            "files": views,
            "artifact": artifact,
        });

        if files.is_empty() {
            result["note"] = json!(match &directory {
                Some(directory) if !task::is_git_work_tree(directory) => format!(
                    "{directory} is not a git work tree, so OpenCode records no snapshots and this diff is empty even if files changed. Read the files or use `messages` instead."
                ),
                _ => "no changes were recorded for this range; the default range is the newest turn only".to_string(),
            });
        }

        pretty(&result)
    }

    #[tool(
        name = "reply_permission",
        description = "Answer a pending OpenCode permission request: reply=once approves just this action, always approves the rule going forward, reject denies it. Include a message with reject to tell the agent what to do instead."
    )]
    async fn reply_permission(
        &self,
        Parameters(args): Parameters<ReplyPermissionArgs>,
    ) -> Result<String, String> {
        let reply = parse_reply(&args.reply)?;
        let job = self
            .maybe_job(args.job_id.as_deref(), args.session_id.as_deref())
            .await?;

        let (session_id, request_id) = match &job {
            Some(job) => {
                let snapshot = job.snapshot().await;
                let request_id = non_empty(args.request_id.as_deref())
                    .map(str::to_string)
                    .or_else(|| snapshot.pending_permission.as_ref().map(|p| p.id.clone()))
                    .ok_or_else(|| {
                        format!(
                            "job {} has no pending permission; call status to see what it is doing",
                            job.id
                        )
                    })?;
                (job.session_id.clone(), request_id)
            }
            None => {
                let session_id = non_empty(args.session_id.as_deref())
                    .ok_or_else(|| "pass job_id or session_id".to_string())?
                    .to_string();
                let request_id = non_empty(args.request_id.as_deref())
                    .ok_or_else(|| {
                        "pass request_id when the job is not tracked by this Gate process"
                            .to_string()
                    })?
                    .to_string();
                (session_id, request_id)
            }
        };

        let pending = self
            .gateway
            .client
            .permissions(&session_id)
            .await
            .map_err(|error| error.to_string())?;
        let request = pending
            .iter()
            .find(|pending| pending.id == request_id)
            .ok_or_else(|| {
                format!("permission {request_id} is no longer pending; refresh status")
            })?;
        if reply != PermissionReply::Reject
            && let crate::policy::Decision::Reject(reason) = self.gateway.policy.decide(request)
        {
            return Err(reason);
        }
        let _operation = match &job {
            Some(job) => Some(job.operation.lock().await),
            None => None,
        };
        self.gateway
            .client
            .reply_permission(&session_id, &request_id, reply, args.message.as_deref())
            .await
            .map_err(|error| error.to_string())?;

        if let Some(job) = &job {
            job.clear_permission(&request_id).await;
        }
        drop(_operation);
        match job {
            Some(job) => {
                self.gateway.dispatcher.reconcile(&job).await;
                pretty(&job_view(&job.snapshot().await))
            }
            None => pretty(&json!({
                "session_id": session_id,
                "request_id": request_id,
                "reply": reply.as_str(),
            })),
        }
    }

    #[tool(
        name = "list",
        description = "List delegated and recovered jobs for this OpenCode backend, newest first, with status, elapsed time and a one-line summary."
    )]
    async fn list(&self) -> Result<String, String> {
        let mut jobs = self.gateway.registry.list().await;
        jobs.reverse();
        let views: Vec<Value> = jobs
            .iter()
            .map(|snapshot| {
                json!({
                    "job_id": snapshot.job_id,
                    "session_id": snapshot.session_id,
                    "status": snapshot.status,
                    "elapsed_ms": snapshot.elapsed_ms,
                    "directory": snapshot.directory,
                    "agent": snapshot.agent,
                    "needs_permission": snapshot.pending_permission.is_some(),
                    "summary": snapshot.summary.as_deref().map(|s| truncate(s, 200)),
                })
            })
            .collect();
        pretty(&json!({ "count": views.len(), "jobs": views }))
    }

    #[tool(
        name = "agents",
        description = "List OpenCode agents available in a directory, with id, mode and description. Use it to choose a specialised agent for `delegate`."
    )]
    async fn agents(&self, Parameters(args): Parameters<AgentsArgs>) -> Result<String, String> {
        let directory = task::resolve_directory(&self.gateway.config, args.directory.as_deref())?;
        let agents = self
            .gateway
            .client
            .agents(Some(&directory))
            .await
            .map_err(|error| error.to_string())?;
        let include_hidden = args.include_hidden.unwrap_or(false);

        let views: Vec<Value> = agents
            .iter()
            .filter(|agent| include_hidden || !agent.hidden)
            .map(|agent| {
                json!({
                    "id": agent.id,
                    "name": agent.name,
                    "mode": agent.mode,
                    "description": agent.description,
                    "hidden": agent.hidden,
                })
            })
            .collect();

        pretty(&json!({ "directory": directory, "agents": views }))
    }

    #[tool(
        name = "models",
        description = "List enabled OpenCode models and variant ids available in a directory. Use provider/model#variant as the model argument to delegate."
    )]
    async fn models(&self, Parameters(args): Parameters<ModelsArgs>) -> Result<String, String> {
        let directory = task::resolve_directory(&self.gateway.config, args.directory.as_deref())?;
        let models = self
            .gateway
            .client
            .models(&directory)
            .await
            .map_err(|error| error.to_string())?;
        let models: Vec<Value> = models.into_iter().filter(|model| model.enabled).map(|model| json!({
            "model": format!("{}/{}", model.provider_id, model.id),
            "name": model.name, "status": model.status,
            "variants": model.variants.into_iter().map(|variant| variant.id).collect::<Vec<_>>(),
        })).collect();
        pretty(&json!({"directory": directory, "models": models}))
    }
}

#[tool_handler]
impl ServerHandler for GateServer {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "gate",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(INSTRUCTIONS)
    }
}

impl GateServer {
    async fn maybe_job(
        &self,
        job_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Option<Arc<Job>>, String> {
        if let Some(job_id) = non_empty(job_id) {
            let job = self
                .gateway
                .registry
                .get(job_id)
                .await
                .ok_or_else(|| format!("unknown job {job_id}; call `list` for job ids"))?;
            if let Some(session_id) = non_empty(session_id)
                && session_id != job.session_id
            {
                return Err("job_id and session_id refer to different sessions".into());
            }
            return Ok(Some(job));
        }
        if let Some(session_id) = non_empty(session_id) {
            if !session_id.starts_with("ses_")
                || !session_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err("invalid session_id".into());
            }
            return Ok(self.gateway.registry.by_session(session_id).await);
        }
        Ok(None)
    }

    async fn require_job(
        &self,
        job_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Arc<Job>, String> {
        if let Some(job) = self.maybe_job(job_id, session_id).await? {
            return Ok(job);
        }
        match (non_empty(job_id), non_empty(session_id)) {
            (Some(job_id), _) => Err(format!("unknown job {job_id}; call `list` for job ids")),
            (None, Some(session_id)) => Err(format!(
                "no job tracked for session {session_id} (it may belong to another Gate process)"
            )),
            (None, None) => Err("pass job_id (or session_id)".to_string()),
        }
    }

    /// Resolve a session id even when no job is tracked, so `messages` and
    /// `diff` can inspect sessions created by an earlier Gate process.
    async fn session_of(
        &self,
        job_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<String, String> {
        if let Some(job) = self.maybe_job(job_id, session_id).await? {
            return Ok(job.session_id.clone());
        }
        match non_empty(session_id) {
            Some(session_id) => Ok(session_id.to_string()),
            None => match non_empty(job_id) {
                Some(job_id) => Err(format!("unknown job {job_id}; call `list` for job ids")),
                None => Err("pass job_id or session_id".to_string()),
            },
        }
    }
}

fn job_view(snapshot: &JobSnapshot) -> Value {
    let mut view = serde_json::to_value(snapshot)
        .unwrap_or_else(|_| json!({ "job_id": snapshot.job_id, "status": snapshot.status }));
    // OpenCode derives diffs from git snapshots; outside a work tree the diff
    // fields are empty even when the agent wrote files.
    view["git"] = json!(task::is_git_work_tree(&snapshot.directory));
    if let Some(summary) = &snapshot.summary {
        view["summary"] = json!(truncate(summary, 8_000));
    }
    view["next"] = json!(next_hint(snapshot));
    view
}

fn next_hint(snapshot: &JobSnapshot) -> String {
    if !snapshot.prompt_admitted
        && snapshot.status == JobStatus::Running
        && snapshot.error.is_some()
    {
        return "prompt admission is uncertain; inspect messages and cancel before retrying".into();
    }
    match snapshot.status {
        JobStatus::Running => format!(
            "call `wait` with job_id={} (budgets up to {} ms) or `status` to peek",
            snapshot.job_id, MAX_WAIT_MS
        ),
        JobStatus::BlockedOnPermission => match &snapshot.pending_permission {
            Some(request) => format!(
                "call `reply_permission` with job_id={}, request_id={} and reply=once|always|reject",
                snapshot.job_id, request.id
            ),
            None => "call `status` to refresh the pending permission".to_string(),
        },
        JobStatus::Completed => {
            "review the changes with `diff`; continue with `delegate` and session_id, or delegate a new task"
                .to_string()
        }
        JobStatus::Failed => {
            "read `messages` for the failure, then continue with `delegate` and session_id or delegate again".to_string()
        }
        JobStatus::Cancelled => {
            "the job was cancelled; changes already on disk remain and can be reviewed with `diff`"
                .to_string()
        }
    }
}

fn parse_delivery(raw: Option<&str>) -> Result<Delivery, String> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(Delivery::Steer),
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "steer" => Ok(Delivery::Steer),
            "queue" => Ok(Delivery::Queue),
            other => Err(format!("delivery must be steer or queue (got {other:?})")),
        },
    }
}

fn parse_reply(raw: &str) -> Result<PermissionReply, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "once" | "allow" => Ok(PermissionReply::Once),
        "always" => Ok(PermissionReply::Always),
        "reject" | "deny" => Ok(PermissionReply::Reject),
        other => Err(format!(
            "reply must be once, always or reject (got {other:?})"
        )),
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn pretty(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map_err(|error| format!("could not render the result: {error}"))
}

/// Clip to `max` characters without splitting a UTF-8 sequence.
fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let clipped: String = value.chars().take(max).collect();
    format!("{clipped}…[truncated]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::registry::Usage;

    fn snapshot(status: JobStatus) -> JobSnapshot {
        JobSnapshot {
            job_id: "job_1".into(),
            session_id: "ses_1".into(),
            directory: "/tmp".into(),
            status,
            agent: Some("build".into()),
            model: None,
            summary: Some("done".into()),
            error: None,
            partial_text: None,
            progress: vec!["shell: cargo test".into()],
            pending_permission: None,
            usage: Usage::default(),
            diff: None,
            artifact: None,
            elapsed_ms: 42,
            created_at_ms: 42,
            worktree: None,
            client: None,
            prompt_admitted: true,
            first_message_id: None,
            last_message_id: None,
        }
    }

    #[test]
    fn completed_snapshot_points_at_diff() {
        let hint = next_hint(&snapshot(JobStatus::Completed));
        assert!(hint.contains("diff"));
    }

    #[test]
    fn blocked_snapshot_names_the_request() {
        let mut snap = snapshot(JobStatus::BlockedOnPermission);
        snap.pending_permission = Some(crate::opencode::types::PermissionRequest {
            id: "per_9".into(),
            session_id: "ses_1".into(),
            action: "shell".into(),
            resources: vec!["cargo publish".into()],
            message: None,
        });
        let hint = next_hint(&snap);
        assert!(hint.contains("per_9"));
        assert!(hint.contains("reply_permission"));
    }

    #[test]
    fn parses_permission_replies() {
        assert_eq!(parse_reply("once").unwrap(), PermissionReply::Once);
        assert_eq!(parse_reply("ALWAYS").unwrap(), PermissionReply::Always);
        assert_eq!(parse_reply("reject").unwrap(), PermissionReply::Reject);
        assert!(parse_reply("maybe").is_err());
    }

    #[test]
    fn delivery_defaults_to_steer() {
        assert_eq!(parse_delivery(None).unwrap(), Delivery::Steer);
        assert_eq!(parse_delivery(Some("queue")).unwrap(), Delivery::Queue);
        assert!(parse_delivery(Some("later")).is_err());
    }

    #[test]
    fn truncation_is_utf8_safe() {
        let clipped = truncate("héllo wörld", 5);
        assert!(clipped.starts_with("héllo"));
    }
}
