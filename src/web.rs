//! `gate web`: a read-only browser view of Gate's projects, jobs and sessions.
//!
//! The page uses two routes. `/api/jobs` returns the shared job records for the
//! configured backend. `/oc/...` forwards GET requests to OpenCode's session and
//! event endpoints with Gate's credentials, so the password never reaches the
//! browser. The server binds to loopback and checks `Host` to block DNS rebinding.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::config::GateConfig;
use crate::opencode::types::AUTH_USER;

const PAGE: &str = include_str!("web.html");
const FRAMEWORK: &str = include_str!("vendor/preact-htm.js");
const MAX_HEAD: usize = 16 * 1024;

pub async fn serve(config: GateConfig, port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let port = listener.local_addr()?.port();
    eprintln!(
        "gate web: http://127.0.0.1:{port}  (OpenCode {})",
        config.base_url
    );
    // No request timeout: `/oc/api/event` is a long-lived SSE stream.
    let http = reqwest::Client::builder()
        .user_agent(concat!("gate/", env!("CARGO_PKG_VERSION")))
        // OpenCode closes idle connections; a reused one fails the next request.
        .pool_max_idle_per_host(0)
        .build()?;
    let config = Arc::new(config);
    loop {
        let (socket, _) = listener.accept().await?;
        let (config, http) = (config.clone(), http.clone());
        tokio::spawn(async move {
            if let Err(error) = handle(socket, port, &config, &http).await {
                tracing::debug!(%error, "web request ended");
            }
        });
    }
}

async fn handle(
    mut socket: TcpStream,
    port: u16,
    config: &GateConfig,
    http: &reqwest::Client,
) -> Result<()> {
    let mut head = Vec::new();
    let mut buffer = [0u8; 4096];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = socket.read(&mut buffer).await?;
        if read == 0 || head.len() > MAX_HEAD {
            return Ok(());
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let path = match route(&head, port) {
        Ok(path) => path,
        Err(status) => return respond(&mut socket, status, "text/plain", status.as_bytes()).await,
    };

    if path == "/" {
        return respond(
            &mut socket,
            "200",
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
        )
        .await;
    }
    if path == "/api/jobs" {
        let body = serde_json::to_vec(&jobs(config))?;
        return respond(&mut socket, "200", "application/json", &body).await;
    }
    if path == "/preact-htm.js" {
        return respond(
            &mut socket,
            "200",
            "text/javascript; charset=utf-8",
            FRAMEWORK.as_bytes(),
        )
        .await;
    }
    let Some(upstream) = path.strip_prefix("/oc") else {
        return respond(&mut socket, "404", "text/plain", b"404").await;
    };
    let mut request = http.get(format!("{}{upstream}", config.base_url));
    if let Some(password) = &config.password {
        request = request.basic_auth(AUTH_USER, Some(password));
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return respond(
                &mut socket,
                "502",
                "text/plain",
                error.to_string().as_bytes(),
            )
            .await;
        }
    };
    let status = response.status().as_u16().to_string();
    let kind = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    socket.write_all(header(&status, &kind).as_bytes()).await?;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        socket.write_all(&chunk?).await?;
    }
    Ok(())
}

/// Accept only loopback `GET`s for the page, the job list, and OpenCode's
/// session and event reads. Returns the request path or an HTTP status.
fn route(head: &str, port: u16) -> std::result::Result<&str, &'static str> {
    let mut lines = head.lines();
    let mut request = lines.next().unwrap_or("").split(' ');
    if request.next() != Some("GET") {
        return Err("405");
    }
    let path = request.next().unwrap_or("");
    let host = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("host").then(|| value.trim())
        })
        .unwrap_or("");
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Err("403");
    }
    let open = path == "/"
        || path == "/api/jobs"
        || path == "/preact-htm.js"
        || path.starts_with("/oc/api/session/")
        || path == "/oc/api/event";
    // The URL parser resolves `%2e%2e` like `..`, so refuse both before forwarding.
    let target = path.split('?').next().unwrap_or("").to_ascii_lowercase();
    if !open || target.contains("..") || target.contains("%2e") {
        return Err("404");
    }
    Ok(path)
}

/// Job records for this backend. Records from other OpenCode URLs name
/// sessions this server cannot read.
fn jobs(config: &GateConfig) -> Vec<serde_json::Value> {
    let Ok(entries) = std::fs::read_dir(&config.state_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            let mut record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            (record["backend"] == config.base_url.as_str()).then(|| record["job"].take())
        })
        .collect()
}

fn header(status: &str, kind: &str) -> String {
    format!(
        "HTTP/1.1 {status} -\r\nContent-Type: {kind}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    )
}

async fn respond(socket: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> Result<()> {
    socket.write_all(header(status, kind).as_bytes()).await?;
    socket.write_all(body).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(path: &str, host: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n")
    }

    #[test]
    fn routes_only_loopback_reads() {
        let ok = get("/oc/api/session/ses_1/message?limit=100", "127.0.0.1:7777");
        assert_eq!(
            route(&ok, 7777),
            Ok("/oc/api/session/ses_1/message?limit=100")
        );
        assert_eq!(route(&get("/", "localhost:7777"), 7777), Ok("/"));
        assert_eq!(
            route(&get("/preact-htm.js", "127.0.0.1:7777"), 7777),
            Ok("/preact-htm.js")
        );
        assert_eq!(
            route(&get("/oc/api/event", "127.0.0.1:7777"), 7777),
            Ok("/oc/api/event")
        );
        assert_eq!(route(&get("/", "evil.example:7777"), 7777), Err("403"));
        assert_eq!(route(&get("/", "127.0.0.1:7777"), 8888), Err("403"));
        assert_eq!(
            route(&get("/oc/api/model", "127.0.0.1:7777"), 7777),
            Err("404")
        );
        assert_eq!(
            route(&get("/oc/api/session/../agent", "127.0.0.1:7777"), 7777),
            Err("404")
        );
        assert_eq!(
            route(&get("/oc/api/session/%2E%2e/agent", "127.0.0.1:7777"), 7777),
            Err("404")
        );
        let post = "POST /oc/api/session/ses_1/prompt HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n";
        assert_eq!(route(post, 7777), Err("405"));
    }
}
