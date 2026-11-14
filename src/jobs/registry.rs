//! Job state: one job is one OpenCode session.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::opencode::types::PermissionRequest;

pub const PROGRESS_LIMIT: usize = 40;
pub const TEXT_TAIL_LIMIT: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    BlockedOnPermission,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::BlockedOnPermission => "blocked_on_permission",
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cost_usd: f64,
}

impl Usage {
    pub fn from_session(info: &crate::opencode::types::SessionInfo) -> Self {
        let tokens = info.tokens.clone().unwrap_or_default();
        Self {
            input: tokens.input,
            output: tokens.output,
            reasoning: tokens.reasoning,
            cost_usd: info.cost.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffStat {
    pub files: u64,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: Vec<String>,
}

impl DiffStat {
    pub fn from_files(files: &[crate::opencode::types::FileDiff]) -> Self {
        let mut stat = Self::default();
        for file in files {
            stat.files += 1;
            stat.additions += file.additions;
            stat.deletions += file.deletions;
            if !file.file.is_empty() {
                stat.changed_files.push(file.file.clone());
            }
        }
        stat
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub session_id: String,
    pub directory: String,
    pub status: JobStatus,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub summary: Option<String>,
    pub error: Option<String>,
    /// Last streamed assistant text, useful while the job is still running.
    pub partial_text: Option<String>,
    pub progress: Vec<String>,
    pub pending_permission: Option<PermissionRequest>,
    pub usage: Usage,
    pub diff: Option<DiffStat>,
    pub artifact: Option<String>,
    pub elapsed_ms: u64,
    pub created_at_ms: u64,
    pub worktree: Option<Worktree>,
    /// MCP client that delegated the job, e.g. `claude-code` or `codex-mcp-client`.
    #[serde(default)]
    pub client: Option<String>,
    pub prompt_admitted: bool,
    pub first_message_id: Option<String>,
    pub last_message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub project_id: String,
    pub source_directory: String,
    pub branch: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    backend: String,
    job: JobSnapshot,
    #[serde(default)]
    baseline_usage: Usage,
}

#[derive(Debug, Clone)]
struct Store {
    directory: PathBuf,
    backend: String,
}

#[derive(Debug)]
struct Inner {
    status: JobStatus,
    agent: Option<String>,
    model: Option<String>,
    summary: Option<String>,
    error: Option<String>,
    partial_text: String,
    progress: VecDeque<String>,
    pending_permission: Option<PermissionRequest>,
    usage: Usage,
    diff: Option<DiffStat>,
    artifact: Option<String>,
    elapsed_ms: Option<u64>,
    prompt_admitted: bool,
    first_message_id: Option<String>,
    last_message_id: Option<String>,
}

#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub session_id: String,
    pub directory: String,
    pub created: Instant,
    pub created_at_ms: u64,
    pub worktree: Option<Worktree>,
    pub client: Option<String>,
    pub recovered: bool,
    pub baseline_usage: Usage,
    store: Option<Store>,
    lease: Option<std::fs::File>,
    /// Serialize finalization and reconciliation for this session, not the firehose.
    pub operation: Mutex<()>,
    inner: Mutex<Inner>,
    changed: Notify,
}

impl Drop for Job {
    fn drop(&mut self) {
        // A child between fork and exec can still hold an inherited descriptor.
        if let Some(lease) = &self.lease {
            let _ = lease.unlock();
        }
    }
}

impl Job {
    pub fn new(
        id: String,
        session_id: String,
        directory: String,
        agent: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            id,
            session_id,
            directory,
            created: Instant::now(),
            created_at_ms: now_ms(),
            worktree: None,
            client: None,
            recovered: false,
            baseline_usage: Usage::default(),
            store: None,
            lease: None,
            operation: Mutex::new(()),
            inner: Mutex::new(Inner {
                status: JobStatus::Running,
                agent,
                model,
                summary: None,
                error: None,
                partial_text: String::new(),
                progress: VecDeque::new(),
                pending_permission: None,
                usage: Usage::default(),
                diff: None,
                artifact: None,
                elapsed_ms: None,
                prompt_admitted: false,
                first_message_id: None,
                last_message_id: None,
            }),
            changed: Notify::new(),
        }
    }

    pub fn new_id() -> String {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        format!("job_{}", &raw[..12])
    }

    pub async fn status(&self) -> JobStatus {
        self.inner.lock().await.status
    }

    pub async fn snapshot(&self) -> JobSnapshot {
        let inner = self.inner.lock().await;
        self.snapshot_inner(&inner)
    }

    fn snapshot_inner(&self, inner: &Inner) -> JobSnapshot {
        JobSnapshot {
            job_id: self.id.clone(),
            session_id: self.session_id.clone(),
            directory: self.directory.clone(),
            status: inner.status,
            agent: inner.agent.clone(),
            model: inner.model.clone(),
            summary: inner.summary.clone(),
            error: inner.error.clone(),
            partial_text: (!inner.partial_text.is_empty()).then(|| inner.partial_text.clone()),
            progress: inner.progress.iter().cloned().collect(),
            pending_permission: inner.pending_permission.clone(),
            usage: inner.usage.clone(),
            diff: inner.diff.clone(),
            artifact: inner.artifact.clone(),
            elapsed_ms: inner
                .elapsed_ms
                .unwrap_or_else(|| now_ms().saturating_sub(self.created_at_ms)),
            created_at_ms: self.created_at_ms,
            worktree: self.worktree.clone(),
            client: self.client.clone(),
            prompt_admitted: inner.prompt_admitted,
            first_message_id: inner.first_message_id.clone(),
            last_message_id: inner.last_message_id.clone(),
        }
    }

    pub async fn mark_admitted(&self) {
        let mut inner = self.inner.lock().await;
        inner.prompt_admitted = true;
        self.persist(&mut inner);
    }

    pub async fn set_error(&self, error: String) {
        let mut inner = self.inner.lock().await;
        inner.error = Some(error);
        self.persist(&mut inner);
        drop(inner);
        self.changed.notify_waiters();
    }

    pub async fn record_prompt(&self, response: &serde_json::Value) {
        let mut inner = self.inner.lock().await;
        if let Some(id) = response.get("id").and_then(serde_json::Value::as_str) {
            inner.first_message_id.get_or_insert_with(|| id.to_string());
            inner.last_message_id = Some(id.to_string());
        }
        inner.prompt_admitted = true;
        self.persist(&mut inner);
    }

    pub async fn update_usage(&self, total: Usage) {
        let mut inner = self.inner.lock().await;
        if inner.status.is_terminal() {
            return;
        }
        inner.usage = Usage {
            input: total.input.saturating_sub(self.baseline_usage.input),
            output: total.output.saturating_sub(self.baseline_usage.output),
            reasoning: total
                .reasoning
                .saturating_sub(self.baseline_usage.reasoning),
            cost_usd: (total.cost_usd - self.baseline_usage.cost_usd).max(0.0),
        };
    }

    fn persist(&self, inner: &mut Inner) {
        if let Some(store) = &self.store
            && let Err(error) = self.save(store, inner)
        {
            let message = format!("job recovery record could not be saved: {error:#}");
            tracing::error!(job = %self.id, %message);
            inner.error = Some(message);
        }
    }

    fn save(&self, store: &Store, inner: &Inner) -> anyhow::Result<()> {
        super::store::write_json(
            &store.directory.join(format!("{}.json", self.id)),
            &Record {
                backend: store.backend.clone(),
                job: self.snapshot_inner(inner),
                baseline_usage: self.baseline_usage.clone(),
            },
        )
    }

    pub async fn set_running(&self) {
        let mut inner = self.inner.lock().await;
        if !inner.status.is_terminal() {
            inner.status = if inner.pending_permission.is_some() {
                JobStatus::BlockedOnPermission
            } else {
                JobStatus::Running
            };
        }
        drop(inner);
        self.changed.notify_waiters();
    }

    pub async fn push_progress(&self, line: impl Into<String>) {
        let mut inner = self.inner.lock().await;
        inner.progress.push_back(line.into());
        while inner.progress.len() > PROGRESS_LIMIT {
            inner.progress.pop_front();
        }
        drop(inner);
        self.changed.notify_waiters();
    }

    pub async fn push_text(&self, delta: &str) {
        let mut inner = self.inner.lock().await;
        inner.partial_text.push_str(delta);
        if inner.partial_text.len() > TEXT_TAIL_LIMIT {
            let overflow = inner.partial_text.len() - TEXT_TAIL_LIMIT;
            let mut cut = overflow;
            while cut < inner.partial_text.len() && !inner.partial_text.is_char_boundary(cut) {
                cut += 1;
            }
            inner.partial_text.drain(..cut);
        }
        drop(inner);
        self.changed.notify_waiters();
    }

    pub async fn set_permission(&self, request: PermissionRequest) {
        let mut inner = self.inner.lock().await;
        if inner.status.is_terminal() {
            return;
        }
        inner.status = JobStatus::BlockedOnPermission;
        inner.pending_permission = Some(request);
        self.persist(&mut inner);
        drop(inner);
        self.changed.notify_waiters();
    }

    /// Record the agent and model the session actually used.
    pub async fn set_agent_model(&self, agent: Option<String>, model: Option<String>) {
        if agent.is_none() && model.is_none() {
            return;
        }
        let mut inner = self.inner.lock().await;
        if agent.is_some() {
            inner.agent = agent;
        }
        if model.is_some() {
            inner.model = model;
        }
    }

    pub async fn clear_permission(&self, request_id: &str) {
        let mut inner = self.inner.lock().await;
        if inner
            .pending_permission
            .as_ref()
            .is_none_or(|pending| pending.id != request_id)
        {
            return;
        }
        inner.pending_permission = None;
        if inner.status == JobStatus::BlockedOnPermission {
            inner.status = JobStatus::Running;
        }
        self.persist(&mut inner);
        drop(inner);
        self.changed.notify_waiters();
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish(
        &self,
        status: JobStatus,
        summary: Option<String>,
        error: Option<String>,
        usage: Usage,
        diff: Option<DiffStat>,
        artifact: Option<String>,
    ) {
        let mut inner = self.inner.lock().await;
        if inner.status.is_terminal() {
            return;
        }
        inner.status = status;
        if summary.is_some() {
            inner.summary = summary;
        }
        inner.error = error;
        inner.usage = usage;
        inner.diff = diff;
        inner.artifact = artifact;
        inner.pending_permission = None;
        inner.elapsed_ms = Some(now_ms().saturating_sub(self.created_at_ms));
        self.persist(&mut inner);
        drop(inner);
        self.changed.notify_waiters();
    }

    /// Wait for the job to finish, to need attention, or for the budget to run out.
    pub async fn wait(&self, budget: Duration, stop_on_permission: bool) -> JobSnapshot {
        let deadline = Instant::now() + budget;

        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            // Register interest before reading state so a change cannot be lost.
            notified.as_mut().enable();

            let snapshot = self.snapshot().await;
            if snapshot.status.is_terminal() {
                return snapshot;
            }
            if stop_on_permission && snapshot.pending_permission.is_some() {
                return snapshot;
            }

            let now = Instant::now();
            if now >= deadline {
                return snapshot;
            }
            let _ = tokio::time::timeout(deadline - now, &mut notified).await;
        }
    }
}

#[derive(Debug, Default)]
pub struct Registry {
    jobs: RwLock<HashMap<String, Arc<Job>>>,
    by_session: RwLock<HashMap<String, String>>,
    store: Option<Store>,
    /// Keep concurrent delegate calls from admitting two prompts to one session.
    pub admission: Mutex<()>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(directory: PathBuf, backend: String) -> anyhow::Result<Self> {
        let store = Store { directory, backend };
        let mut jobs = HashMap::new();
        let mut sessions = HashMap::new();
        if store.directory.exists() {
            for entry in std::fs::read_dir(&store.directory)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Some(lease) = super::store::lease(&path.with_extension("lock"))? else {
                    continue;
                };
                let record: Record =
                    serde_json::from_slice(&std::fs::read(&path)?).map_err(|error| {
                        anyhow::anyhow!("invalid recovery record {}: {error}", path.display())
                    })?;
                if record.backend != store.backend {
                    continue;
                }
                let saved = record.job;
                anyhow::ensure!(
                    saved.job_id.starts_with("job_")
                        && saved.job_id[4..].chars().all(|c| c.is_ascii_hexdigit())
                        && saved.job_id.len() > 4,
                    "invalid job id in {}",
                    path.display()
                );
                anyhow::ensure!(
                    path.file_stem().and_then(|name| name.to_str()) == Some(saved.job_id.as_str()),
                    "job id does not match record path {}",
                    path.display()
                );
                let mut job = Job::new(
                    saved.job_id.clone(),
                    saved.session_id.clone(),
                    saved.directory,
                    saved.agent,
                    saved.model,
                );
                job.created_at_ms = saved.created_at_ms;
                job.worktree = saved.worktree;
                job.client = saved.client;
                job.store = Some(store.clone());
                job.lease = Some(lease);
                job.recovered = true;
                job.baseline_usage = record.baseline_usage;
                let inner = job.inner.get_mut();
                inner.status = saved.status;
                inner.summary = saved.summary;
                inner.error = saved.error;
                inner.partial_text = saved.partial_text.unwrap_or_default();
                inner.progress = saved.progress.into();
                inner.pending_permission = saved.pending_permission;
                inner.usage = saved.usage;
                inner.diff = saved.diff;
                inner.artifact = saved.artifact;
                inner.elapsed_ms = saved.status.is_terminal().then_some(saved.elapsed_ms);
                inner.prompt_admitted = saved.prompt_admitted;
                inner.first_message_id = saved.first_message_id;
                inner.last_message_id = saved.last_message_id;
                let latest = sessions
                    .entry(saved.session_id)
                    .or_insert_with(|| (0, String::new()));
                if saved.created_at_ms >= latest.0 {
                    *latest = (saved.created_at_ms, saved.job_id.clone());
                }
                jobs.insert(saved.job_id, Arc::new(job));
            }
        }
        Ok(Self {
            jobs: RwLock::new(jobs),
            by_session: RwLock::new(
                sessions
                    .into_iter()
                    .map(|(session, (_, id))| (session, id))
                    .collect(),
            ),
            store: Some(store),
            admission: Mutex::new(()),
        })
    }

    /// Persist before exposing the job or admitting its prompt.
    pub async fn register(&self, mut job: Job) -> anyhow::Result<Arc<Job>> {
        if let Some(store) = &self.store {
            job.lease = Some(
                super::store::lease(&store.directory.join(format!("{}.lock", job.id)))?
                    .ok_or_else(|| {
                        anyhow::anyhow!("job {} is owned by another Gate process", job.id)
                    })?,
            );
            job.save(store, &*job.inner.lock().await)?;
            job.store = Some(store.clone());
        }
        let job = Arc::new(job);
        self.insert(job.clone()).await;
        Ok(job)
    }

    pub async fn insert(&self, job: Arc<Job>) {
        self.by_session
            .write()
            .await
            .insert(job.session_id.clone(), job.id.clone());
        self.jobs.write().await.insert(job.id.clone(), job);
    }

    pub async fn get(&self, job_id: &str) -> Option<Arc<Job>> {
        self.jobs.read().await.get(job_id).cloned()
    }

    pub async fn by_session(&self, session_id: &str) -> Option<Arc<Job>> {
        let id = self.by_session.read().await.get(session_id)?.clone();
        self.jobs.read().await.get(&id).cloned()
    }

    pub async fn list(&self) -> Vec<JobSnapshot> {
        let jobs: Vec<_> = self.jobs.read().await.values().cloned().collect();
        let mut snapshots = Vec::with_capacity(jobs.len());
        for job in jobs {
            snapshots.push(job.snapshot().await);
        }
        snapshots.sort_by_key(|a| a.created_at_ms);
        snapshots
    }

    pub async fn running(&self) -> Vec<Arc<Job>> {
        let jobs: Vec<_> = self.jobs.read().await.values().cloned().collect();
        let mut running = Vec::new();
        for job in jobs {
            if !job.status().await.is_terminal() {
                running.push(job);
            }
        }
        running
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Arc<Job> {
        Arc::new(Job::new(
            "job_test".into(),
            "ses_test".into(),
            "/tmp".into(),
            Some("build".into()),
            None,
        ))
    }

    #[tokio::test]
    async fn wait_returns_immediately_when_finished() {
        let job = job();
        job.finish(
            JobStatus::Completed,
            Some("done".into()),
            None,
            Usage::default(),
            None,
            None,
        )
        .await;
        let snapshot = job.wait(Duration::from_secs(5), true).await;
        assert_eq!(snapshot.status, JobStatus::Completed);
        assert_eq!(snapshot.summary.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn wait_wakes_on_finish() {
        let job = job();
        let waiter = job.clone();
        let handle = tokio::spawn(async move { waiter.wait(Duration::from_secs(5), true).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        job.finish(
            JobStatus::Failed,
            None,
            Some("boom".into()),
            Usage::default(),
            None,
            None,
        )
        .await;
        let snapshot = handle.await.unwrap();
        assert_eq!(snapshot.status, JobStatus::Failed);
        assert_eq!(snapshot.error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn wait_stops_on_permission_when_asked() {
        let job = job();
        let waiter = job.clone();
        let handle = tokio::spawn(async move { waiter.wait(Duration::from_secs(5), true).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        job.set_permission(PermissionRequest {
            id: "per_1".into(),
            session_id: "ses_test".into(),
            action: "shell".into(),
            resources: vec!["cargo test".into()],
            message: None,
        })
        .await;
        let snapshot = handle.await.unwrap();
        assert_eq!(snapshot.status, JobStatus::BlockedOnPermission);
        assert!(snapshot.pending_permission.is_some());
    }

    #[tokio::test]
    async fn progress_is_bounded() {
        let job = job();
        for i in 0..(PROGRESS_LIMIT + 10) {
            job.push_progress(format!("step {i}")).await;
        }
        let snapshot = job.snapshot().await;
        assert_eq!(snapshot.progress.len(), PROGRESS_LIMIT);
        assert!(snapshot.progress.last().unwrap().contains("49"));
    }

    #[tokio::test]
    async fn recovery_keeps_state_and_only_claims_unowned_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path().into(), "http://backend".into()).unwrap();
        let mut new = Job::new(Job::new_id(), "ses_test".into(), "/tmp".into(), None, None);
        new.client = Some("codex-mcp-client".into());
        let job = registry.register(new).await.unwrap();
        let id = job.id.clone();
        job.mark_admitted().await;
        job.finish(
            JobStatus::Completed,
            Some("done".into()),
            None,
            Usage::default(),
            None,
            None,
        )
        .await;
        let elapsed = job.snapshot().await.elapsed_ms;
        let other = Registry::open(dir.path().into(), "http://backend".into()).unwrap();
        assert!(other.list().await.is_empty());
        drop(other);
        drop(job);
        drop(registry);
        let registry = Registry::open(dir.path().into(), "http://other-backend".into()).unwrap();
        assert!(registry.list().await.is_empty());
        drop(registry);
        let registry = Registry::open(dir.path().into(), "http://backend".into()).unwrap();
        let saved = registry.get(&id).await.unwrap().snapshot().await;
        assert_eq!(saved.status, JobStatus::Completed);
        assert_eq!(saved.summary.as_deref(), Some("done"));
        assert_eq!(saved.elapsed_ms, elapsed);
        assert!(saved.prompt_admitted);
        assert_eq!(saved.client.as_deref(), Some("codex-mcp-client"));
    }

    #[tokio::test]
    async fn permission_replies_do_not_clear_a_newer_request_or_reopen_finished_jobs() {
        let job = job();
        let request = PermissionRequest {
            id: "per_new".into(),
            session_id: job.session_id.clone(),
            action: "edit".into(),
            resources: vec![],
            message: None,
        };
        job.set_permission(request.clone()).await;
        job.clear_permission("per_old").await;
        assert_eq!(
            job.snapshot().await.pending_permission.unwrap().id,
            "per_new"
        );
        job.clear_permission("per_new").await;
        assert_eq!(job.status().await, JobStatus::Running);
        job.finish(
            JobStatus::Cancelled,
            None,
            None,
            Usage::default(),
            None,
            None,
        )
        .await;
        job.set_permission(request).await;
        assert!(job.snapshot().await.pending_permission.is_none());
    }

    #[tokio::test]
    async fn running_usage_excludes_prior_turns_and_saturates_after_a_backend_reset() {
        let mut job = Job::new(Job::new_id(), "ses_test".into(), "/tmp".into(), None, None);
        job.baseline_usage = Usage {
            input: 10,
            output: 2,
            reasoning: 1,
            cost_usd: 0.5,
        };
        job.update_usage(Usage {
            input: 15,
            output: 5,
            reasoning: 2,
            cost_usd: 0.7,
        })
        .await;
        let usage = job.snapshot().await.usage;
        assert_eq!((usage.input, usage.output, usage.reasoning), (5, 3, 1));
        assert!((usage.cost_usd - 0.2).abs() < 1e-10);
        job.update_usage(Usage::default()).await;
        assert_eq!(job.snapshot().await.usage.input, 0);
        assert_eq!(job.snapshot().await.usage.cost_usd, 0.0);
    }

    #[test]
    fn corrupt_recovery_data_is_reported_without_overwriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job_dead.json");
        std::fs::write(&path, b"broken").unwrap();
        assert!(Registry::open(dir.path().into(), "http://backend".into()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken");
    }
}
