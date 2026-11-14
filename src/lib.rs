//! Gate — an MCP gateway that exposes OpenCode as a coding backend.
//!
//! Claude Code and Codex act as planner/reviewer; Gate maps their delegated
//! tasks onto durable OpenCode sessions, streams progress back, and bridges
//! permission prompts.

pub mod config;
pub mod jobs;
pub mod mcp;
pub mod opencode;
pub mod policy;
pub mod web;

use std::sync::Arc;

use anyhow::Context;

use crate::config::GateConfig;
use crate::jobs::dispatcher::Dispatcher;
use crate::jobs::registry::Registry;
use crate::opencode::client::OpenCode;
use crate::opencode::events::EventBus;
use crate::policy::Policy;

/// Everything the MCP tools need, wired once at startup.
pub struct Gateway {
    pub config: Arc<GateConfig>,
    pub client: Arc<OpenCode>,
    pub registry: Arc<Registry>,
    pub policy: Arc<Policy>,
    pub dispatcher: Arc<Dispatcher>,
}

impl Gateway {
    pub fn new(config: GateConfig) -> anyhow::Result<Arc<Self>> {
        let config = Arc::new(config);
        let client = Arc::new(
            OpenCode::new(&config.base_url, config.password.clone())
                .context("building OpenCode HTTP client")?,
        );
        let registry = Arc::new(
            Registry::open(config.state_dir.clone(), config.base_url.clone())
                .context("loading job recovery records")?,
        );
        let policy = Arc::new(Policy::new(config.permission_mode));

        let bus = EventBus::spawn(client.clone(), registry.clone());
        let dispatcher = Dispatcher::spawn(
            registry.clone(),
            client.clone(),
            policy.clone(),
            config.clone(),
            bus,
        );

        Ok(Arc::new(Self {
            config,
            client,
            registry,
            policy,
            dispatcher,
        }))
    }

    /// Directory a delegated task should run in when the caller omits one.
    pub fn default_directory(&self) -> String {
        self.config.default_directory.display().to_string()
    }
}
