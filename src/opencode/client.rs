//! Thin async client over the OpenCode HTTP API.

use std::time::Duration;

use bytes::Bytes;
use futures_util::Stream;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::{
    AUTH_USER, AgentInfo, CreateSessionRequest, FileDiff, Location, LocationInfo, Message,
    ModelInfo, ModelRef, PermissionReply, PermissionRequest, PromptRequest, ServerInfo, Session,
    SessionInfo,
};

#[derive(Debug, thiserror::Error)]
pub enum OpenCodeError {
    #[error("opencode connection failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("opencode returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("could not decode opencode response: {detail}")]
    Decode { detail: String },
}

impl OpenCodeError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub fn is_unauthorized(&self) -> bool {
        self.status() == Some(StatusCode::UNAUTHORIZED.as_u16())
    }
}

pub type Result<T> = std::result::Result<T, OpenCodeError>;

#[derive(serde::Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(serde::Deserialize)]
struct MessagePage {
    data: Vec<Message>,
    #[serde(default)]
    cursor: MessageCursor,
}

#[derive(Default, serde::Deserialize)]
struct MessageCursor {
    next: Option<String>,
}

#[derive(Clone)]
pub struct OpenCode {
    base: String,
    password: Option<String>,
    http: reqwest::Client,
}

impl OpenCode {
    pub fn new(base_url: &str, password: Option<String>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("gate/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            password,
            http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let builder = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .timeout(Duration::from_secs(60));
        match &self.password {
            Some(password) => builder.basic_auth(AUTH_USER, Some(password)),
            None => builder,
        }
    }

    async fn json<T: DeserializeOwned>(&self, builder: RequestBuilder) -> Result<T> {
        let response = builder.send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(OpenCodeError::Status {
                status: status.as_u16(),
                body: truncate(&body, 2_000),
            });
        }
        serde_json::from_str(&body).map_err(|error| OpenCodeError::Decode {
            detail: format!("{error}; body={}", truncate(&body, 500)),
        })
    }

    async fn empty(&self, builder: RequestBuilder) -> Result<()> {
        let response = builder.send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.text().await.unwrap_or_default();
        Err(OpenCodeError::Status {
            status: status.as_u16(),
            body: truncate(&body, 2_000),
        })
    }

    pub async fn info(&self) -> Result<ServerInfo> {
        self.json(self.request(Method::GET, "/api/info")).await
    }

    pub async fn create_session(&self, request: &CreateSessionRequest) -> Result<Session> {
        let envelope: Envelope<Session> = self
            .json(self.request(Method::POST, "/api/session").json(request))
            .await?;
        Ok(envelope.data)
    }

    /// Most recent sessions, newest first. Used by `gate doctor`.
    pub async fn sessions(&self, limit: Option<u32>) -> Result<Vec<Session>> {
        let mut builder = self.request(Method::GET, "/api/session");
        if let Some(limit) = limit {
            builder = builder.query(&[("limit", limit.to_string()), ("order", "desc".into())]);
        }
        let envelope: Envelope<Vec<Session>> = self.json(builder).await?;
        Ok(envelope.data)
    }

    pub async fn session(&self, session_id: &str) -> Result<SessionInfo> {
        let envelope: Envelope<SessionInfo> = self
            .json(self.request(Method::GET, &format!("/api/session/{session_id}")))
            .await?;
        Ok(envelope.data)
    }

    /// Sessions with a foreground drain in this OpenCode process. Sessions
    /// absent from the result are inactive.
    pub async fn active_sessions(&self) -> Result<std::collections::HashSet<String>> {
        let envelope: Envelope<std::collections::HashMap<String, Value>> = self
            .json(self.request(Method::GET, "/api/session/active"))
            .await?;
        Ok(envelope.data.into_keys().collect())
    }

    pub async fn set_agent(&self, session_id: &str, agent: &str) -> Result<()> {
        self.empty(
            self.request(Method::POST, &format!("/api/session/{session_id}/agent"))
                .json(&serde_json::json!({ "agent": agent })),
        )
        .await
    }

    pub async fn set_model(&self, session_id: &str, model: &ModelRef) -> Result<()> {
        self.empty(
            self.request(Method::POST, &format!("/api/session/{session_id}/model"))
                .json(&serde_json::json!({ "model": model })),
        )
        .await
    }

    pub async fn prompt(&self, session_id: &str, request: &PromptRequest) -> Result<Value> {
        let envelope: Envelope<Value> = self
            .json(
                self.request(Method::POST, &format!("/api/session/{session_id}/prompt"))
                    .json(request),
            )
            .await?;
        Ok(envelope.data)
    }

    /// Block until the session's agent loop is idle. Used by `gate doctor` and
    /// as a fallback; normal waits go through the event bus.
    pub async fn wait_idle(&self, session_id: &str, timeout: Duration) -> Result<()> {
        self.empty(
            self.request(
                Method::POST,
                &format!("/api/experimental/session/{session_id}/wait"),
            )
            .timeout(timeout),
        )
        .await
    }

    pub async fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut messages = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let mut builder = self
                .request(Method::GET, &format!("/api/session/{session_id}/message"))
                .query(&[("limit", "100")]);
            builder = match &cursor {
                Some(cursor) => builder.query(&[("cursor", cursor)]),
                None => builder.query(&[("order", "asc")]),
            };
            let page: MessagePage = self.json(builder).await?;
            messages.extend(page.data);
            match page.cursor.next {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => {
                    return Err(OpenCodeError::Decode {
                        detail: "message pagination cursor repeated".into(),
                    });
                }
                None => return Ok(messages),
            }
        }
    }

    pub async fn latest_user_message(&self, session_id: &str) -> Result<Option<Message>> {
        let envelope: Envelope<Vec<Message>> = self
            .json(
                self.request(Method::GET, &format!("/api/session/{session_id}/message"))
                    .query(&[("type", "user"), ("limit", "1"), ("order", "desc")]),
            )
            .await?;
        Ok(envelope.data.into_iter().next())
    }

    /// Newest-first message page, used by the `messages` tool.
    pub async fn message_page(
        &self,
        session_id: &str,
        limit: Option<u32>,
        order: Option<&str>,
    ) -> Result<Vec<Message>> {
        let mut builder = self.request(Method::GET, &format!("/api/session/{session_id}/message"));
        if let Some(limit) = limit {
            builder = builder.query(&[("limit", limit.to_string())]);
        }
        if let Some(order) = order {
            builder = builder.query(&[("order", order)]);
        }
        let envelope: Envelope<Vec<Message>> = self.json(builder).await?;
        Ok(envelope.data)
    }

    pub async fn diff(&self, session_id: &str) -> Result<Vec<FileDiff>> {
        self.diff_with_context(session_id, None).await
    }

    pub async fn diff_with_context(
        &self,
        session_id: &str,
        context: Option<u32>,
    ) -> Result<Vec<FileDiff>> {
        self.diff_range(session_id, context, None, None).await
    }

    pub async fn diff_range(
        &self,
        session_id: &str,
        context: Option<u32>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<FileDiff>> {
        let mut builder = self.request(Method::GET, &format!("/api/session/{session_id}/diff"));
        if let Some(context) = context {
            builder = builder.query(&[("context", context.to_string())]);
        }
        if let Some(from) = from {
            builder = builder.query(&[("from", from)]);
        }
        if let Some(to) = to {
            builder = builder.query(&[("to", to)]);
        }
        let envelope: Envelope<Vec<FileDiff>> = self.json(builder).await?;
        Ok(envelope.data)
    }

    pub async fn interrupt(&self, session_id: &str) -> Result<()> {
        self.interrupt_with_resume(session_id, true).await
    }

    /// `resume=false` stops the turn instead of replaying pending steering input.
    pub async fn interrupt_with_resume(&self, session_id: &str, resume: bool) -> Result<()> {
        self.empty(
            self.request(
                Method::POST,
                &format!("/api/session/{session_id}/interrupt"),
            )
            .query(&[("resume", if resume { "true" } else { "false" })]),
        )
        .await
    }

    pub async fn permissions(&self, session_id: &str) -> Result<Vec<PermissionRequest>> {
        let envelope: Envelope<Vec<PermissionRequest>> = self
            .json(self.request(
                Method::GET,
                &format!("/api/session/{session_id}/permission"),
            ))
            .await?;
        Ok(envelope.data)
    }

    pub async fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        reply: PermissionReply,
        message: Option<&str>,
    ) -> Result<()> {
        // The wire field is `decision` (verified against the live 2.1.4
        // OpenAPI: the body is `{decision, message?}`, not `{reply}`).
        let mut body = serde_json::json!({ "decision": reply.as_str() });
        if let Some(message) = message.filter(|m| !m.trim().is_empty()) {
            body["message"] = serde_json::Value::String(message.to_string());
        }
        self.empty(
            self.request(
                Method::POST,
                &format!("/api/session/{session_id}/permission/{request_id}/reply"),
            )
            .json(&body),
        )
        .await
    }

    pub async fn agents(&self, directory: Option<&str>) -> Result<Vec<AgentInfo>> {
        let mut builder = self.request(Method::GET, "/api/agent");
        if let Some(directory) = directory {
            builder = builder.query(&[("location[directory]", directory)]);
        }
        let envelope: Envelope<Vec<AgentInfo>> = self.json(builder).await?;
        Ok(envelope.data)
    }

    pub async fn models(&self, directory: &str) -> Result<Vec<ModelInfo>> {
        let envelope: Envelope<Vec<ModelInfo>> = self
            .json(
                self.request(Method::GET, "/api/model")
                    .query(&[("location[directory]", directory)]),
            )
            .await?;
        Ok(envelope.data)
    }

    pub async fn location(&self, directory: &str) -> Result<LocationInfo> {
        self.json(
            self.request(Method::GET, "/api/location")
                .query(&[("location[directory]", directory)]),
        )
        .await
    }

    pub async fn create_worktree(
        &self,
        project_id: &str,
        from: &str,
        name: &str,
    ) -> Result<Location> {
        self.json(
            self.request(Method::POST, "/api/worktree")
                .json(&serde_json::json!({
                "projectID": project_id, "from": from, "name": name,
                })),
        )
        .await
    }

    /// Open the global SSE firehose. Callers must filter by session and location.
    pub async fn event_stream(
        &self,
    ) -> Result<impl Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static>
    {
        let builder = self.http.get(format!("{}/api/event", self.base));
        let builder = match &self.password {
            Some(password) => builder.basic_auth(AUTH_USER, Some(password)),
            None => builder,
        };
        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(OpenCodeError::Status {
                status: status.as_u16(),
                body: truncate(&body, 2_000),
            });
        }
        Ok(response.bytes_stream())
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated {} bytes]", &value[..end], value.len() - end)
}
