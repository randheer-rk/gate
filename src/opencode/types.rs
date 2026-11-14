//! Types mirroring the OpenCode HTTP API.

use serde::{Deserialize, Serialize};

pub const AUTH_USER: &str = "opencode";

/// `provider/model` plus an optional `#variant` suffix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl ModelRef {
    /// Parse `provider/model#variant`.
    ///
    /// The model id may itself contain slashes (`ollama/hf.co/...`), so only the
    /// first slash separates provider from model.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("model must not be empty".into());
        }
        let (spec, variant) = match raw.rsplit_once('#') {
            Some((spec, variant)) => (spec, Some(variant.to_string())),
            None => (raw, None),
        };
        let (provider_id, id) = spec.split_once('/').ok_or_else(|| {
            format!("model {raw:?} must be written as provider/model or provider/model#variant")
        })?;
        if provider_id.is_empty() || id.is_empty() {
            return Err(format!("model {raw:?} has an empty provider or model id"));
        }
        Ok(Self {
            id: id.to_string(),
            provider_id: provider_id.to_string(),
            variant: variant.filter(|v| !v.is_empty()),
        })
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider_id, self.id)?;
        if let Some(variant) = &self.variant {
            write!(f, "#{variant}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub directory: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "parentID", default)]
    pub parent_id: Option<String>,
}

/// `GET /api/session/{id}`: enough to reconcile a job after a reconnect.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    /// `succeeded`, `failed` or `interrupted` once a turn has finished.
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<ModelRef>,
    #[serde(default)]
    pub time: Option<SessionTime>,
    pub location: Location,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub tokens: Option<TokenUsage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionTime {
    #[serde(default)]
    pub updated: Option<i64>,
    #[serde(default)]
    pub idle: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    pub location: Location,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(rename = "parentID", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptRequest {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<FileAttachment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// Inject into the running turn.
    Steer,
    /// Deliver after the current turn.
    Queue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAttachment {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub content: Vec<serde_json::Value>,
    /// User messages carry their text at the top level, not in `content`.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<ModelRef>,
    #[serde(default)]
    pub tokens: Option<TokenUsage>,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub finish: Option<String>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Message {
    /// Concatenate every text block, falling back to the top-level `text` that
    /// user messages use. Reasoning and tool blocks stay in full artifacts.
    pub fn text(&self) -> String {
        let joined = self
            .content
            .iter()
            .filter(|block| block.get("type").and_then(|kind| kind.as_str()) == Some("text"))
            .filter_map(|block| block.get("text").and_then(|text| text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        if joined.is_empty() {
            self.text.clone().unwrap_or_default()
        } else {
            joined
        }
    }

    /// One line per tool call: name plus its most identifiable input field.
    pub fn tool_calls(&self) -> Vec<String> {
        self.content
            .iter()
            .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool"))
            .map(|block| {
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                let input = block
                    .get("state")
                    .and_then(|state| state.get("input"))
                    .unwrap_or(&serde_json::Value::Null);
                let detail = ["command", "filePath", "path", "pattern", "query"]
                    .iter()
                    .find_map(|key| input.get(*key).and_then(|v| v.as_str()))
                    .unwrap_or("");
                if detail.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}({detail})")
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileDiff {
    pub file: String,
    #[serde(default)]
    pub patch: String,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub action: String,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionReply {
    Once,
    Always,
    Reject,
}

impl PermissionReply {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Always => "always",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocationInfo {
    pub directory: String,
    pub project: ProjectLocation,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectLocation {
    pub id: String,
    pub canonical: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    pub name: String,
    pub enabled: bool,
    pub status: String,
    #[serde(default)]
    pub variants: Vec<ModelVariant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVariant {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub urls: Vec<String>,
}

/// One SSE event from `GET /api/event`.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub created: Option<i64>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub data: serde_json::Value,
}

impl Event {
    pub fn session_id(&self) -> Option<&str> {
        self.data.get("sessionID").and_then(|v| v.as_str())
    }

    pub fn directory(&self) -> Option<&str> {
        self.location.as_ref().map(|l| l.directory.as_str())
    }

    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(|v| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_model() {
        let model = ModelRef::parse("anthropic/claude-sonnet-5-5").unwrap();
        assert_eq!(model.provider_id, "anthropic");
        assert_eq!(model.id, "claude-sonnet-5-5");
        assert_eq!(model.variant, None);
    }

    #[test]
    fn parses_variant() {
        let model = ModelRef::parse("anthropic/claude-sonnet-5-5#high").unwrap();
        assert_eq!(model.variant.as_deref(), Some("high"));
        assert_eq!(model.to_string(), "anthropic/claude-sonnet-5-5#high");
    }

    #[test]
    fn keeps_slashes_in_model_id() {
        let model =
            ModelRef::parse("ollama/hf.co/Fortytwo-Network/Strand-Rust-Coder-14B-v1-GGUF:Q5_K_M")
                .unwrap();
        assert_eq!(model.provider_id, "ollama");
        assert_eq!(
            model.id,
            "hf.co/Fortytwo-Network/Strand-Rust-Coder-14B-v1-GGUF:Q5_K_M"
        );
    }

    #[test]
    fn rejects_missing_provider() {
        assert!(ModelRef::parse("claude-sonnet-5-5").is_err());
    }

    #[test]
    fn summaries_exclude_reasoning_and_artifacts_keep_unrecognized_metadata() {
        let value = serde_json::json!({
            "id": "msg_1", "type": "assistant", "time": {"created": 42},
            "content": [{"type": "reasoning", "text": "internal"}, {"type": "text", "text": "answer"}],
            "tokens": {"input": 10, "cache": {"read": 5}},
        });
        let message: Message = serde_json::from_value(value).unwrap();
        assert_eq!(message.text(), "answer");
        let archived = serde_json::to_value(message).unwrap();
        assert_eq!(archived["time"]["created"], 42);
        assert_eq!(archived["tokens"]["cache"]["read"], 5);
    }
}
