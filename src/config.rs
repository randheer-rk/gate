//! Configuration and OpenCode service discovery.
//!
//! OpenCode registers its running server in
//! `$XDG_STATE_HOME/opencode/service.json` (usually
//! `~/.local/state/opencode/service.json`) with the URL and a Basic-auth
//! password. That file is the primary discovery source; environment variables
//! override it and a fixed default is used as a last resort.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::policy::PermissionMode;

pub const DEFAULT_URL: &str = "http://127.0.0.1:4096";
pub const DEFAULT_WAIT_MS: u64 = 45_000;

#[derive(Debug, Clone)]
pub struct GateConfig {
    /// Base URL of the OpenCode HTTP API, e.g. `http://127.0.0.1:49374`.
    pub base_url: String,
    /// Basic-auth password. Auth user is always `opencode`.
    pub password: Option<String>,
    /// Directory delegated tasks run in when the caller does not specify one.
    pub default_directory: PathBuf,
    /// OpenCode agent id used for delegated sessions.
    pub default_agent: Option<String>,
    /// OpenCode model in `provider/model#variant` form.
    pub default_model: Option<String>,
    /// How long `delegate` waits before returning a job id.
    pub default_wait_ms: u64,
    /// What to do with OpenCode permission requests.
    pub permission_mode: PermissionMode,
    /// Where full transcripts and patches are written.
    pub artifact_dir: PathBuf,
    /// Durable job records, shared by Gate processes using the same backend.
    pub state_dir: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct ServiceRegistration {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pid: Option<u32>,
}

impl GateConfig {
    pub fn discover() -> Result<Self> {
        let state = read_json::<ServiceRegistration>(&state_service_path()).unwrap_or_default();
        let config = read_json::<ServiceRegistration>(&config_service_path()).unwrap_or_default();

        let base_url = env_first(&["GATE_OPENCODE_URL", "OPENCODE_URL"])
            .or(state.url)
            .or(config.url)
            .unwrap_or_else(|| DEFAULT_URL.to_string());

        let password = env_first(&["GATE_OPENCODE_PASSWORD", "OPENCODE_PASSWORD"])
            .or(state.password)
            .or(config.password);

        let default_directory = env_first(&["GATE_DIRECTORY", "CLAUDE_PROJECT_DIR"])
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .context("resolving working directory")?;

        let default_wait_ms = parse_wait(env_first(&["GATE_WAIT_MS"]).as_deref())?;

        let permission_mode = env_first(&["GATE_PERMISSION_MODE"])
            .map(|v| v.parse().map_err(anyhow::Error::msg))
            .transpose()?
            .unwrap_or_default();

        let artifact_dir = env_first(&["GATE_ARTIFACT_DIR"])
            .map(PathBuf::from)
            .unwrap_or_else(|| state_home().join("gate").join("artifacts"));

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            password,
            default_directory,
            default_agent: env_first(&["GATE_AGENT"]),
            default_model: env_first(&["GATE_MODEL"]),
            default_wait_ms,
            permission_mode,
            artifact_dir,
            state_dir: env_first(&["GATE_STATE_DIR"])
                .map(PathBuf::from)
                .unwrap_or_else(|| state_home().join("gate").join("jobs")),
        })
    }

    /// Redacted, serialisable view for `gate doctor`.
    pub fn redacted(&self) -> serde_json::Value {
        serde_json::json!({
            "opencode_url": self.base_url,
            "password_configured": self.password.is_some(),
            "default_directory": self.default_directory,
            "default_agent": self.default_agent,
            "default_model": self.default_model,
            "default_wait_ms": self.default_wait_ms,
            "permission_mode": self.permission_mode,
            "artifact_dir": self.artifact_dir,
            "state_dir": self.state_dir,
        })
    }
}

fn parse_wait(value: Option<&str>) -> Result<u64> {
    value
        .map_or(Ok(DEFAULT_WAIT_MS), |value| {
            value
                .parse::<u64>()
                .context("GATE_WAIT_MS must be a non-negative integer")
        })
        .map(|wait| wait.min(crate::jobs::task::MAX_WAIT_MS))
}

pub fn state_home() -> PathBuf {
    env_first(&["XDG_STATE_HOME"])
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local").join("state"))
}

pub fn config_home() -> PathBuf {
    env_first(&["XDG_CONFIG_HOME"])
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

fn state_service_path() -> PathBuf {
    state_home().join("opencode").join("service.json")
}

fn config_service_path() -> PathBuf {
    config_home().join("opencode").join("service.json")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_first(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_never_leaks_password() {
        let cfg = GateConfig {
            base_url: DEFAULT_URL.into(),
            password: Some("super-secret".into()),
            default_directory: PathBuf::from("/tmp"),
            default_agent: None,
            default_model: None,
            default_wait_ms: DEFAULT_WAIT_MS,
            permission_mode: PermissionMode::Auto,
            artifact_dir: PathBuf::from("/tmp/artifacts"),
            state_dir: PathBuf::from("/tmp/jobs"),
        };
        let rendered = cfg.redacted().to_string();
        assert!(!rendered.contains("super-secret"));
        assert!(rendered.contains("\"password_configured\":true"));
    }

    #[test]
    fn trailing_slash_is_trimmed() {
        let url = "http://127.0.0.1:4096/".trim_end_matches('/').to_string();
        assert_eq!(url, "http://127.0.0.1:4096");
    }

    #[test]
    fn wait_config_rejects_invalid_values_and_caps_budgets() {
        assert!(parse_wait(Some("bad")).is_err());
        assert!(parse_wait(Some("-1")).is_err());
        assert_eq!(parse_wait(None).unwrap(), DEFAULT_WAIT_MS);
        assert_eq!(parse_wait(Some("0")).unwrap(), 0);
        assert_eq!(parse_wait(Some("999999")).unwrap(), 120_000);
    }
}
