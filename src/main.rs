//! `gate` CLI: serve MCP over stdio, or diagnose the OpenCode connection.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use serde_json::json;

use gate::Gateway;
use gate::config::GateConfig;
use gate::mcp::GateServer;

#[derive(Debug, Parser)]
#[command(
    name = "gate",
    version,
    about = "MCP gateway that exposes OpenCode as a coding backend for Claude Code and Codex",
    long_about = "Gate speaks MCP on stdin/stdout and turns delegated tasks into durable OpenCode \n\
                  sessions. Run without a subcommand to serve; run `gate doctor` to check discovery."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the MCP protocol on stdin/stdout (the default).
    Mcp,
    /// Check service discovery, authentication, agents and the event stream.
    Doctor {
        /// Print a machine-readable report instead of lines.
        #[arg(long)]
        json: bool,
    },
    /// Serve a read-only browser view of projects, jobs, transcripts and diffs.
    Web {
        /// Loopback port to listen on.
        #[arg(long, default_value_t = 7777)]
        port: u16,
    },
}

fn main() -> ExitCode {
    init_tracing();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("gate: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("gate: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode> {
    match Cli::parse().command.unwrap_or(Command::Mcp) {
        Command::Mcp => {
            serve().await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor { json } => doctor(json).await,
        Command::Web { port } => {
            gate::web::serve(GateConfig::discover()?, port).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// stdout belongs to the MCP protocol, so every log line goes to stderr.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("gate=info,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

async fn serve() -> Result<()> {
    let config = GateConfig::discover()?;
    tracing::info!(
        url = %config.base_url,
        directory = %config.default_directory.display(),
        permission_mode = %config.permission_mode,
        "starting gate MCP server on stdio"
    );

    let gateway = Gateway::new(config)?;
    let server = GateServer::new(gateway);

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("initialising the MCP server on stdio")?;
    tracing::info!("MCP client connected");

    service
        .waiting()
        .await
        .context("the MCP server task failed")?;
    tracing::info!("MCP client disconnected; shutting down");
    Ok(())
}

async fn doctor(json_output: bool) -> Result<ExitCode> {
    let config = match GateConfig::discover() {
        Ok(config) => config,
        Err(error) if json_output => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": false, "checks": [], "failures": [format!("configuration: {error:#}")],
                }))?
            );
            return Ok(ExitCode::FAILURE);
        }
        Err(error) => return Err(error),
    };
    let redacted = config.redacted();
    let default_directory = config.default_directory.display().to_string();
    let client = gate::opencode::client::OpenCode::new(&config.base_url, config.password)?;

    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    match client.info().await {
        Ok(info) => checks.push(json!({
            "check": "opencode",
            "ok": true,
            "version": info.version,
            "pid": info.pid,
            "urls": info.urls,
        })),
        Err(error) => {
            failures.push(format!("opencode: {error}"));
            checks.push(json!({"check": "opencode", "ok": false, "error": error.to_string()}));
        }
    }

    match client.agents(Some(&default_directory)).await {
        Ok(agents) => {
            let visible: Vec<&str> = agents
                .iter()
                .filter(|agent| !agent.hidden)
                .map(|agent| agent.id.as_str())
                .collect();
            checks.push(json!({
                "check": "agents",
                "ok": true,
                "count": visible.len(),
                "ids": visible,
            }));
        }
        Err(error) => {
            failures.push(format!("agents: {error}"));
            checks.push(json!({"check": "agents", "ok": false, "error": error.to_string()}));
        }
    }

    match client.sessions(Some(1)).await {
        Ok(sessions) => checks.push(json!({
            "check": "sessions",
            "ok": true,
            "reachable": true,
            "sample": sessions.first().map(|session| session.id.clone()),
        })),
        Err(error) => {
            failures.push(format!("sessions: {error}"));
            checks.push(json!({"check": "sessions", "ok": false, "error": error.to_string()}));
        }
    }

    // Opening the stream is the health check; a quiet firehose sends nothing
    // until some session does work, so do not wait for a frame.
    let stream = tokio::time::timeout(Duration::from_secs(5), client.event_stream()).await;
    match stream {
        Ok(Ok(_stream)) => checks.push(json!({"check": "event_stream", "ok": true})),
        Ok(Err(error)) => {
            failures.push(format!("event stream: {error}"));
            checks.push(json!({"check": "event_stream", "ok": false, "error": error.to_string()}));
        }
        Err(_) => {
            failures.push("event stream: timed out opening the SSE connection".to_string());
            checks.push(json!({
                "check": "event_stream",
                "ok": false,
                "error": "timed out after 5s",
            }));
        }
    }

    let ok = failures.is_empty();

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": ok,
                "config": redacted,
                "checks": checks,
                "failures": failures,
            }))?
        );
    } else {
        println!(
            "opencode  {}  (directory {})",
            redacted["opencode_url"].as_str().unwrap_or("?"),
            default_directory
        );
        for check in &checks {
            let name = check["check"].as_str().unwrap_or("?");
            match check["ok"].as_bool().unwrap_or(false) {
                true => {
                    let detail = check["ids"]
                        .as_array()
                        .map(|ids| {
                            ids.iter()
                                .filter_map(|id| id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .or_else(|| {
                            check["version"]
                                .as_str()
                                .map(|version| format!("v{version}"))
                        })
                        .unwrap_or_default();
                    println!("[ok]   {name:<12} {detail}");
                }
                false => println!(
                    "[fail] {name:<12} {}",
                    check["error"].as_str().unwrap_or("failed")
                ),
            }
        }
        if ok {
            println!("\ngate is ready. Launch it from an MCP client with `gate mcp`.");
        } else {
            println!("\n{} check(s) failed.", failures.len());
        }
    }

    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
