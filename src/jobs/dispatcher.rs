//! Route the global event stream without blocking unrelated jobs on HTTP calls.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::broadcast;

use crate::config::GateConfig;
use crate::jobs::registry::{DiffStat, Job, JobStatus, Registry, Usage};
use crate::opencode::client::OpenCode;
use crate::opencode::events::{EventBus, RESYNC_EVENT};
use crate::opencode::types::{Event, Message, PermissionReply, PermissionRequest};
use crate::policy::{Decision, Policy};

pub struct Dispatcher {
    registry: Arc<Registry>,
    client: Arc<OpenCode>,
    policy: Arc<Policy>,
    config: Arc<GateConfig>,
}

impl Dispatcher {
    pub fn spawn(
        registry: Arc<Registry>,
        client: Arc<OpenCode>,
        policy: Arc<Policy>,
        config: Arc<GateConfig>,
        bus: Arc<EventBus>,
    ) -> Arc<Self> {
        let dispatcher = Arc::new(Self {
            registry,
            client,
            policy,
            config,
        });
        // Subscribe before returning: the first prompt can complete immediately.
        let mut receiver = bus.subscribe();
        let worker = dispatcher.clone();
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => worker.handle(event).await,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "event bus lagged; resynchronising jobs");
                        worker.resync().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let worker = dispatcher.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                worker.resync().await;
            }
        });
        dispatcher
    }

    async fn handle(self: &Arc<Self>, event: Event) {
        if event.kind == RESYNC_EVENT {
            self.resync().await;
            return;
        }
        let Some(session_id) = event.session_id() else {
            return;
        };
        let Some(job) = self.registry.by_session(session_id).await else {
            return;
        };
        if job.status().await.is_terminal()
            || event
                .directory()
                .is_some_and(|directory| directory != job.directory)
        {
            return;
        }
        match event.kind.as_str() {
            "session.execution.started" => job.set_running().await,
            "session.text.delta" => {
                if let Some(delta) = event.str_field("delta") {
                    job.push_text(delta).await;
                }
            }
            "session.tool.called" => job.push_progress(tool_line(&event)).await,
            "session.tool.failed" => job.push_progress(tool_failure(&event)).await,
            "permission.asked" => match serde_json::from_value::<PermissionRequest>(event.data) {
                Ok(request) => {
                    job.set_permission(request.clone()).await;
                    let worker = self.clone();
                    tokio::spawn(async move {
                        let _operation = job.operation.lock().await;
                        worker.answer_permission(&job, request).await;
                    });
                }
                Err(error) => tracing::warn!(%error, "could not decode permission request"),
            },
            "permission.replied" => {
                if let Some(id) = event
                    .str_field("requestID")
                    .or_else(|| event.str_field("id"))
                {
                    job.clear_permission(id).await;
                }
            }
            "session.execution.succeeded"
            | "session.execution.failed"
            | "session.execution.interrupted"
            | "session.idle" => {
                if event.kind == "session.execution.failed" {
                    let detail = event
                        .data
                        .get("error")
                        .map(|error| truncate(&error.to_string(), 800))
                        .unwrap_or_else(|| "OpenCode execution failed".into());
                    job.set_error(detail).await;
                }
                let worker = self.clone();
                tokio::spawn(async move {
                    worker.reconcile(&job).await;
                });
            }
            _ => {}
        }
    }

    async fn answer_permission(&self, job: &Job, request: PermissionRequest) {
        if job.status().await.is_terminal() {
            return;
        }
        let (reply, reason) = match self.policy.decide(&request) {
            Decision::Ask => {
                let current = job.snapshot().await.pending_permission;
                if current.as_ref() != Some(&request) {
                    job.push_progress(format!(
                        "permission required: {} {:?}",
                        request.action, request.resources
                    ))
                    .await;
                    job.set_permission(request).await;
                }
                return;
            }
            Decision::Allow => (PermissionReply::Once, None),
            Decision::Reject(reason) => (PermissionReply::Reject, Some(reason)),
        };
        match self
            .client
            .reply_permission(&request.session_id, &request.id, reply, reason.as_deref())
            .await
        {
            Ok(()) => {
                job.clear_permission(&request.id).await;
                job.push_progress(format!(
                    "permission {}: {} {:?}",
                    reply.as_str(),
                    request.action,
                    request.resources
                ))
                .await;
            }
            Err(error) => {
                job.push_progress(format!("permission reply failed: {error}"))
                    .await;
                job.set_permission(request).await;
            }
        }
    }

    /// Finalization is serialized per job so completion cannot overwrite cancellation.
    pub async fn finalize(&self, job: &Arc<Job>, status: JobStatus, error: Option<String>) {
        let _operation = job.operation.lock().await;
        self.finish_locked(job, status, error).await;
    }

    async fn finish_locked(&self, job: &Arc<Job>, status: JobStatus, mut error: Option<String>) {
        if job.status().await.is_terminal() {
            return;
        }
        let mut messages = Vec::new();
        let snapshot = job.snapshot().await;
        if status == JobStatus::Failed && error.is_none() {
            error = snapshot.error.clone();
        }
        for attempt in 0..4 {
            match self.client.messages(&job.session_id).await {
                Ok(page) => {
                    messages = page;
                    if let Some(id) = &snapshot.first_message_id
                        && let Some(start) = messages.iter().position(|message| &message.id == id)
                    {
                        messages.drain(..start);
                    }
                    if messages.iter().any(|message| {
                        message.kind == "assistant" && !message.text().trim().is_empty()
                    }) {
                        break;
                    }
                }
                Err(failure) => {
                    if attempt == 3 {
                        error.get_or_insert_with(|| {
                            format!("could not retrieve transcript: {failure}")
                        });
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let summary = messages
            .iter()
            .rev()
            .find(|message| message.kind == "assistant" && !message.text().trim().is_empty())
            .map(Message::text);
        let mut usage = Usage::default();
        for message in messages
            .iter()
            .filter(|message| message.kind == "assistant")
        {
            let cost = usage_from(message);
            usage.input += cost.input;
            usage.output += cost.output;
            usage.reasoning += cost.reasoning;
            usage.cost_usd += cost.cost_usd;
        }
        if let Some(message) = messages
            .iter()
            .rev()
            .find(|message| message.kind == "assistant")
        {
            job.set_agent_model(
                message.agent.clone(),
                message.model.as_ref().map(ToString::to_string),
            )
            .await;
            if status == JobStatus::Failed && error.is_none() {
                error = message
                    .error
                    .as_ref()
                    .map(|error| truncate(&error.to_string(), 800));
            }
        }
        if status == JobStatus::Failed && error.is_none() {
            error =
                Some("OpenCode reported a failed execution; inspect messages for details".into());
        }
        let diff = match self
            .client
            .diff_range(
                &job.session_id,
                None,
                snapshot.first_message_id.as_deref(),
                snapshot.last_message_id.as_deref(),
            )
            .await
        {
            Ok(diff) => Some(diff),
            Err(failure) => {
                error.get_or_insert_with(|| format!("could not retrieve diff: {failure}"));
                None
            }
        };
        let snapshot = job.snapshot().await;
        let path = self.config.artifact_dir.join(format!("{}.json", job.id));
        let artifact = match super::store::write_json(
            &path,
            &serde_json::json!({
                "job_id": job.id, "session_id": job.session_id, "directory": job.directory,
                "status": status, "summary": summary, "error": error,
                "agent": snapshot.agent, "model": snapshot.model, "usage": usage,
                "worktree": snapshot.worktree, "elapsed_ms": snapshot.elapsed_ms,
                "messages": messages, "diff": diff,
            }),
        ) {
            Ok(()) => Some(path.display().to_string()),
            Err(failure) => {
                error.get_or_insert_with(|| format!("could not write artifact: {failure:#}"));
                None
            }
        };
        job.finish(
            status,
            summary,
            error,
            usage,
            diff.as_deref().map(DiffStat::from_files),
            artifact,
        )
        .await;
    }

    async fn resync(self: &Arc<Self>) {
        // Network reconciliation runs per job; a stalled backend must not stall the firehose.
        for job in self.registry.running().await {
            let worker = self.clone();
            tokio::spawn(async move {
                worker.reconcile(&job).await;
            });
        }
    }

    pub async fn reconcile(&self, job: &Arc<Job>) {
        let Ok(_operation) = job.operation.try_lock() else {
            return;
        };
        if job.status().await.is_terminal() {
            return;
        }
        if !job.recovered && !job.snapshot().await.prompt_admitted {
            return;
        }
        let active = match self.client.active_sessions().await {
            Ok(active) => active,
            Err(error) => {
                tracing::debug!(%error, "could not list active sessions");
                return;
            }
        };
        let info = match self.client.session(&job.session_id).await {
            Ok(info) => info,
            Err(error) if error.status() == Some(404) => {
                self.finish_locked(
                    job,
                    JobStatus::Failed,
                    Some("OpenCode session no longer exists".into()),
                )
                .await;
                return;
            }
            Err(error) => {
                tracing::debug!(%error, "could not read session state");
                return;
            }
        };
        job.update_usage(Usage::from_session(&info)).await;
        // A permission-blocked session may be absent from the active drain list.
        match self.client.permissions(&job.session_id).await {
            Ok(pending) => {
                if let Some(request) = pending.into_iter().next() {
                    if !job.snapshot().await.prompt_admitted {
                        job.mark_admitted().await;
                    }
                    self.answer_permission(job, request).await;
                    return;
                }
                if let Some(pending) = job.snapshot().await.pending_permission {
                    job.clear_permission(&pending.id).await;
                }
            }
            Err(error) => {
                tracing::debug!(%error, "could not list pending permissions");
                return;
            }
        }
        if !job.snapshot().await.prompt_admitted {
            if !job.recovered {
                return;
            }
            if !active.contains(&job.session_id) {
                self.finish_locked(job, JobStatus::Failed, Some("Gate stopped before prompt admission was confirmed; inspect messages before retrying".into())).await;
                return;
            }
            job.mark_admitted().await;
        }
        if active.contains(&job.session_id) {
            return;
        }
        let status = match info.outcome.as_deref() {
            Some("succeeded") => JobStatus::Completed,
            Some("failed") => JobStatus::Failed,
            Some("interrupted") => JobStatus::Cancelled,
            _ => return,
        };
        self.finish_locked(job, status, None).await;
    }
}

fn usage_from(message: &Message) -> Usage {
    let mut usage = Usage {
        cost_usd: message.cost.unwrap_or(0.0),
        ..Usage::default()
    };
    if let Some(tokens) = &message.tokens {
        usage.input = tokens.input;
        usage.output = tokens.output;
        usage.reasoning = tokens.reasoning;
    }
    usage
}

fn tool_line(event: &Event) -> String {
    let input = event.data.get("input").unwrap_or(&Value::Null);
    match input {
        Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(Value::as_str) {
                return format!("shell: {}", truncate(command.trim(), 160));
            }
            for key in ["filePath", "path", "pattern", "query", "url"] {
                if let Some(value) = map.get(key).and_then(Value::as_str) {
                    return format!("{key}: {}", truncate(value.trim(), 160));
                }
            }
            format!(
                "tool input: {}",
                map.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        }
        Value::Null => "tool called".to_string(),
        other => format!("tool input: {}", truncate(&other.to_string(), 160)),
    }
}

fn tool_failure(event: &Event) -> String {
    let detail = event
        .data
        .get("error")
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown error".to_string());
    format!("tool failed: {}", truncate(&detail, 200))
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let clipped: String = value.chars().take(max).collect();
    format!("{clipped}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencode::types::FileDiff;
    use serde_json::json;

    fn event(kind: &str, data: Value) -> Event {
        Event {
            id: Some("evt_1".into()),
            created: None,
            kind: kind.into(),
            location: None,
            data,
        }
    }

    #[test]
    fn shell_progress_prefers_command() {
        let line = tool_line(&event(
            "session.tool.called",
            json!({"sessionID": "ses_1", "input": {"command": "cargo test --all"}}),
        ));
        assert_eq!(line, "shell: cargo test --all");
    }

    #[test]
    fn file_progress_prefers_path() {
        let line = tool_line(&event(
            "session.tool.called",
            json!({"sessionID": "ses_1", "input": {"filePath": "src/main.rs"}}),
        ));
        assert_eq!(line, "filePath: src/main.rs");
    }

    #[test]
    fn diff_stat_totals() {
        let diff = vec![
            FileDiff {
                file: "a.rs".into(),
                patch: String::new(),
                additions: 3,
                deletions: 1,
                status: "modified".into(),
            },
            FileDiff {
                file: "b.rs".into(),
                patch: String::new(),
                additions: 5,
                deletions: 0,
                status: "added".into(),
            },
        ];
        let stat = DiffStat::from_files(&diff);
        assert_eq!(stat.files, 2);
        assert_eq!(stat.additions, 8);
        assert_eq!(stat.deletions, 1);
        assert_eq!(stat.changed_files, vec!["a.rs", "b.rs"]);
    }
}
