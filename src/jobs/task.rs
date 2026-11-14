//! Starting, steering and stopping delegated tasks.
//!
//! One task is one OpenCode session. The job is registered *before* the first
//! prompt is sent so no event can arrive for a session Gate does not know.

use std::path::PathBuf;
use std::time::Duration;

use crate::config::GateConfig;
use crate::jobs::dispatcher::Dispatcher;
use crate::jobs::registry::{Job, JobSnapshot, JobStatus, Registry, Usage, Worktree};
use crate::opencode::client::{OpenCode, OpenCodeError};
use crate::opencode::types::{
    CreateSessionRequest, Delivery, FileAttachment, Location, ModelRef, PromptRequest,
};

/// Upper bound for a single `wait` call. MCP clients often cap tool calls at
/// 60s (Codex defaults to exactly that), so callers should poll instead of
/// asking for a long block.
pub const MAX_WAIT_MS: u64 = 120_000;

#[derive(Debug, Clone, Default)]
pub struct StartRequest {
    pub task: String,
    pub directory: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub files: Vec<FileAttachment>,
    pub wait: Duration,
    pub session_id: Option<String>,
    pub worktree: bool,
    /// Name the MCP client gave at initialization, kept for attribution.
    pub client: Option<String>,
}

/// Create the session, register the job, send the prompt and wait for a bit.
///
/// Errors only cover problems that leave no job behind (bad arguments, session
/// creation failure). A failed prompt is reported through the snapshot so the
/// caller still learns the session id and can inspect or retry it.
pub async fn start(
    client: &OpenCode,
    registry: &Registry,
    config: &GateConfig,
    dispatcher: &Dispatcher,
    request: StartRequest,
) -> Result<JobSnapshot, String> {
    let task = request.task.trim();
    if task.is_empty() {
        return Err("task must not be empty".to_string());
    }

    if request.worktree && request.session_id.is_some() {
        return Err("worktree cannot be combined with session_id; the existing session already has a directory".into());
    }
    let admission = registry.admission.lock().await;
    let existing = match &request.session_id {
        Some(session_id) => {
            if !session_id.starts_with("ses_")
                || !session_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err("invalid session_id".into());
            }
            if client
                .active_sessions()
                .await
                .map_err(describe)?
                .contains(session_id)
            {
                return Err("session is active; use steer or queue instead".into());
            }
            if !client
                .permissions(session_id)
                .await
                .map_err(describe)?
                .is_empty()
            {
                return Err(
                    "session has pending permissions; answer them before continuing".into(),
                );
            }
            if let Some(job) = registry.by_session(session_id).await
                && !job.status().await.is_terminal()
            {
                return Err("the previous job is still finalizing; wait for it before continuing the session".into());
            }
            Some(client.session(session_id).await.map_err(describe)?)
        }
        None => None,
    };
    let requested_directory = request.directory.as_deref().or_else(|| {
        existing
            .as_ref()
            .map(|session| session.location.directory.as_str())
    });
    let mut directory = resolve_directory(config, requested_directory)?;
    if let Some(session) = &existing
        && directory != session.location.directory
    {
        return Err(format!(
            "session {} belongs to {}, not {directory}",
            session.id, session.location.directory
        ));
    }
    let agent = request.agent.or_else(|| config.default_agent.clone());
    let model = match request.model.or_else(|| config.default_model.clone()) {
        Some(raw) => Some(ModelRef::parse(&raw)?),
        None => None,
    };
    let title = request
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| title_from(task));

    let job_id = Job::new_id();
    let worktree = if request.worktree {
        let location = client.location(&directory).await.map_err(describe)?;
        if location.project.id == "global" {
            return Err("worktree isolation requires a git repository with a commit".into());
        }
        let isolated = client
            .create_worktree(&location.project.id, &directory, &job_id)
            .await
            .map_err(describe)?;
        let source_directory = directory;
        directory = isolated.directory;
        let branch = std::process::Command::new("git")
            .args(["-C", &directory, "branch", "--show-current"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|branch| branch.trim().to_string())
            .filter(|branch| !branch.is_empty());
        Some(Worktree {
            project_id: location.project.id,
            source_directory,
            branch,
        })
    } else {
        None
    };

    let previous_user = match &existing {
        Some(session) => client
            .latest_user_message(&session.id)
            .await
            .map_err(describe)?
            .map(|message| message.id),
        None => None,
    };
    let baseline_usage = existing
        .as_ref()
        .map(Usage::from_session)
        .unwrap_or_default();
    let session_id = if let Some(session) = existing {
        if let Some(agent) = &agent {
            client
                .set_agent(&session.id, agent)
                .await
                .map_err(describe)?;
        }
        if let Some(model) = &model {
            client
                .set_model(&session.id, model)
                .await
                .map_err(describe)?;
        }
        session.id
    } else {
        client
            .create_session(&CreateSessionRequest {
                title: Some(title),
                agent: agent.clone(),
                model: model.clone(),
                location: Location {
                    directory: directory.clone(),
                },
                metadata: Some(serde_json::json!({ "gate": { "job_id": job_id } })),
                parent_id: None,
            })
            .await
            .map_err(|error| match &worktree {
                Some(_) => format!("{error}; the new worktree remains at {directory}"),
                None => describe(error),
            })?
            .id
    };

    let mut job = Job::new(
        job_id,
        session_id.clone(),
        directory.clone(),
        agent,
        model.map(|model| model.to_string()),
    );
    job.worktree = worktree;
    job.client = request.client.clone();
    job.baseline_usage = baseline_usage;
    let job = registry.register(job).await.map_err(|error| {
        format!("could not save job before starting session {session_id} in {directory}: {error:#}")
    })?;
    job.push_progress(format!("session {session_id} admitted"))
        .await;

    let prompt = PromptRequest {
        text: task.to_string(),
        files: (!request.files.is_empty()).then_some(request.files),
        delivery: None,
    };

    match client.prompt(&session_id, &prompt).await {
        Ok(response) => job.record_prompt(&response).await,
        Err(error) => {
            job.push_progress(format!("prompt failed: {error}")).await;
            if error.status().is_none_or(|status| status >= 500) {
                // A lost response does not mean the server rejected the prompt.
                match client.latest_user_message(&session_id).await {
                    Ok(Some(message)) if Some(&message.id) != previous_user.as_ref() && message.text() == task => {
                        job.record_prompt(&serde_json::json!({"id": message.id})).await;
                    }
                    _ => job.set_error(format!("prompt admission could not be confirmed: {error}; inspect messages and cancel before retrying")).await,
                }
                return Ok(job.snapshot().await);
            }
            dispatcher
                .finalize(
                    &job,
                    JobStatus::Failed,
                    Some(format!("could not start the task: {error}")),
                )
                .await;
            return Ok(job.snapshot().await);
        }
    }

    drop(admission);
    Ok(job.wait(request.wait, true).await)
}

/// Send more input to a running job.
pub async fn steer(
    client: &OpenCode,
    job: &Job,
    text: &str,
    delivery: Delivery,
) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("text must not be empty".to_string());
    }
    let _operation = job.operation.lock().await;
    if job.status().await.is_terminal() {
        return Err("job already finished; continue with delegate and session_id".into());
    }
    if !job.snapshot().await.prompt_admitted {
        return Err(
            "prompt admission is uncertain; inspect messages and cancel before sending more input"
                .into(),
        );
    }
    let response = client
        .prompt(
            &job.session_id,
            &PromptRequest {
                text: text.to_string(),
                files: None,
                delivery: Some(delivery),
            },
        )
        .await
        .map_err(describe)?;
    job.record_prompt(&response).await;
    job.push_progress(format!("steering input sent ({delivery:?})"))
        .await;
    Ok(())
}

/// Interrupt the running turn and record the job as cancelled.
///
/// The caller finalizes the job after the server acknowledges interruption.
/// Changes already written to disk are never reverted.
pub async fn cancel(client: &OpenCode, job: &Job) -> Result<(), String> {
    if job.status().await.is_terminal() {
        return Ok(());
    }

    client
        .interrupt_with_resume(&job.session_id, false)
        .await
        .map_err(describe)?;

    Ok(())
}

/// Resolve the directory a task should run in.
///
/// Relative paths are resolved against Gate's default directory; the result
/// must exist because OpenCode indexes a location before creating a session.
pub fn resolve_directory(config: &GateConfig, requested: Option<&str>) -> Result<String, String> {
    let path = match requested.map(str::trim).filter(|dir| !dir.is_empty()) {
        Some(dir) => {
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                path
            } else {
                config.default_directory.join(path)
            }
        }
        None => config.default_directory.clone(),
    };

    if !path.exists() {
        return Err(format!("directory {} does not exist", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }

    Ok(std::fs::canonicalize(&path)
        .unwrap_or(path)
        .display()
        .to_string())
}

/// Does git consider this directory (or one of its ancestors) a work tree?
///
/// OpenCode builds session diffs from git snapshots, so outside a repository
/// `diff` is empty even when files were written. Gate reports this instead of
/// letting a planner mistake "no diff" for "no changes".
pub fn is_git_work_tree(directory: &str) -> bool {
    std::process::Command::new("git")
        .args(["-C", directory, "rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout == b"true\n")
}

fn title_from(task: &str) -> String {
    let first = task.lines().next().unwrap_or(task).trim();
    let mut title: String = first.chars().take(72).collect();
    if first.chars().count() > 72 {
        title.push('…');
    }
    format!("gate: {title}")
}

fn describe(error: OpenCodeError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(directory: &str) -> GateConfig {
        GateConfig {
            base_url: "http://127.0.0.1:4096".into(),
            password: None,
            default_directory: PathBuf::from(directory),
            default_agent: None,
            default_model: None,
            default_wait_ms: 45_000,
            permission_mode: crate::policy::PermissionMode::Auto,
            artifact_dir: PathBuf::from("/tmp/gate-artifacts"),
            state_dir: PathBuf::from("/tmp/gate-jobs"),
        }
    }

    #[test]
    fn rejects_missing_directory() {
        let error = resolve_directory(&config("/tmp"), Some("/definitely/not/here")).unwrap_err();
        assert!(error.contains("does not exist"));
    }

    #[test]
    fn resolves_relative_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("sub");
        std::fs::create_dir(&nested).unwrap();
        let resolved =
            resolve_directory(&config(&dir.path().display().to_string()), Some("sub")).unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(&nested)
                .unwrap()
                .display()
                .to_string()
        );
    }

    #[test]
    fn falls_back_to_default_directory() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_directory(&config(&dir.path().display().to_string()), None).unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path())
                .unwrap()
                .display()
                .to_string()
        );
    }

    #[test]
    fn titles_are_shortened() {
        let title = title_from("fix the parser\nand then everything else");
        assert_eq!(title, "gate: fix the parser");
        let long = title_from(&"a".repeat(120));
        assert_eq!(long.chars().count(), "gate: ".len() + 72 + 1);
    }

    #[test]
    fn detects_git_work_trees_in_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(!is_git_work_tree(&nested.display().to_string()));
        let output = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(is_git_work_tree(&nested.display().to_string()));
    }
}
