//! Permission policy: what Gate does when OpenCode asks for approval.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::opencode::types::PermissionRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Approve everything that is not explicitly denied.
    #[default]
    Auto,
    /// Never answer; surface every request to the orchestrator.
    Ask,
    /// Approve reads and edits, but surface shell commands for review.
    Guarded,
}

impl FromStr for PermissionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "ask" => Ok(Self::Ask),
            "guarded" | "deny-shell" | "deny_shell" => Ok(Self::Guarded),
            other => Err(format!(
                "unknown permission mode {other:?}; expected auto, ask or guarded"
            )),
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Guarded => "guarded",
        };
        f.write_str(name)
    }
}

/// What Gate should do with one permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Reply `once`.
    Allow,
    /// Leave it pending and surface it through `status`/`wait`.
    Ask,
    /// Reply `reject` with a reason.
    Reject(String),
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub mode: PermissionMode,
    /// Shell prefixes that are always rejected, regardless of mode.
    pub deny_shell_prefixes: Vec<String>,
}

impl Policy {
    pub fn new(mode: PermissionMode) -> Self {
        let configured = std::env::var("GATE_DENY_SHELL")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| {
                ["git push", "sudo ", "rm -rf /", "shutdown", "reboot"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            });

        Self {
            mode,
            deny_shell_prefixes: configured,
        }
    }

    pub fn decide(&self, request: &PermissionRequest) -> Decision {
        let shell = matches!(request.action.as_str(), "shell" | "bash");
        if shell {
            for resource in &request.resources {
                if let Some(prefix) = self
                    .deny_shell_prefixes
                    .iter()
                    .find(|prefix| resource.trim_start().starts_with(prefix.as_str()))
                {
                    return Decision::Reject(format!(
                        "shell command {resource:?} matches denied prefix {prefix:?}"
                    ));
                }
            }
        }

        match self.mode {
            PermissionMode::Auto => Decision::Allow,
            PermissionMode::Ask => Decision::Ask,
            PermissionMode::Guarded if shell => Decision::Ask,
            PermissionMode::Guarded => Decision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(action: &str, resources: &[&str]) -> PermissionRequest {
        PermissionRequest {
            id: "per_1".into(),
            session_id: "ses_1".into(),
            action: action.into(),
            resources: resources.iter().map(|s| s.to_string()).collect(),
            message: None,
        }
    }

    #[test]
    fn auto_allows_edits() {
        let policy = Policy::new(PermissionMode::Auto);
        assert_eq!(
            policy.decide(&request("edit", &["src/main.rs"])),
            Decision::Allow
        );
    }

    #[test]
    fn deny_list_wins_over_auto() {
        let policy = Policy::new(PermissionMode::Auto);
        assert!(matches!(
            policy.decide(&request("shell", &["git push origin main"])),
            Decision::Reject(_)
        ));
    }

    #[test]
    fn guarded_surfaces_shell() {
        let policy = Policy::new(PermissionMode::Guarded);
        assert_eq!(
            policy.decide(&request("shell", &["cargo test"])),
            Decision::Ask
        );
        assert_eq!(
            policy.decide(&request("edit", &["src/lib.rs"])),
            Decision::Allow
        );
    }

    #[test]
    fn ask_never_answers() {
        let policy = Policy::new(PermissionMode::Ask);
        assert_eq!(policy.decide(&request("read", &[".env"])), Decision::Ask);
    }

    #[test]
    fn bash_alias_obeys_guarded_mode_and_denied_prefixes() {
        let policy = Policy::new(PermissionMode::Guarded);
        assert_eq!(
            policy.decide(&request("bash", &["cargo test"])),
            Decision::Ask
        );
        assert!(matches!(
            policy.decide(&request("bash", &["git push origin main"])),
            Decision::Reject(_)
        ));
    }

    #[test]
    fn mode_parsing() {
        assert_eq!(
            "auto".parse::<PermissionMode>().unwrap(),
            PermissionMode::Auto
        );
        assert_eq!(
            "guarded".parse::<PermissionMode>().unwrap(),
            PermissionMode::Guarded
        );
        assert!("nonsense".parse::<PermissionMode>().is_err());
    }
}
