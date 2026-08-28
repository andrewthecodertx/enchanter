//! Web chat UI — a local HTTP+SSE front end for one in-process AgentSession.
//!
//! `enchanter serve` hosts a single self-contained HTML page (no asset
//! pipeline, vanilla JS) backed by the same agent events the daemon and REPL
//! use (crate::protocol::Event), streamed to the browser as SSE.
//!
//! The web UI's shape (one page, status bar, streaming chat, collapsible tool
//! calls, model/session switchers) is informed by the web views in Claude Code
//! (claude-code web UI) and OpenCode (`opencode` TUI + web views); the
//! server-side design mirrors the REPL: one `Option<AgentSession>` guarded by a
//! mutex, taken for the duration of a turn and restored when it completes.
//!
//! Security: binds 127.0.0.1 by default and has no authentication. It is a
//! local development interface — do not expose the port publicly.

use std::convert::Infallible;
use std::sync::Arc;

use anyhow::Result;
use colored::Colorize;
use futures_util::Stream;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_stream::wrappers::BroadcastStream;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::agent::{AgentSession, SessionOptions};
use crate::api::TokenUsage;
use crate::config::{Config, ResolvedModel};
use crate::kstore::KnowledgeStore;
use crate::memory::MemoryStore;
use crate::protocol::Event;
use crate::session::{Session, SessionEntry};
use crate::skills::SkillsIndex;
use crate::soul::Soul;

/// Shared server state. The agent is `Option` so a turn can be taken out of
/// the mutex, run to completion, and be restored — the REPL's pattern. During
/// a turn the agent lock is held by the streaming task; read-only endpoints
/// fall back to the cached snapshot so the UI's status poll never blocks.
struct WebState {
    config: Config,
    soul: Soul,
    memory: MemoryStore,
    kstore: KnowledgeStore,
    skills: SkillsIndex,
    resolved: ResolvedModel,
    agent: Mutex<Option<AgentSession>>,
    /// Abort signal for the in-flight turn, if any. `Some` also marks the
    /// server as busy so concurrent /api/chat calls get a 409.
    abort: Mutex<Option<oneshot::Sender<()>>>,
    /// Last computed status snapshot, for endpoints that can't take the lock
    /// while a turn is streaming.
    snapshot: Mutex<Value>,
    /// Cached skills + MCP summaries, for /api/resources when the agent lock
    /// is held by a streaming turn.
    resources: Mutex<Value>,
}

#[derive(serde::Deserialize)]
struct ChatReq {
    prompt: String,
}

#[derive(serde::Deserialize)]
struct ModelReq {
    name: String,
}

#[derive(serde::Deserialize)]
struct ResumeReq {
    session_id: String,
}

#[derive(serde::Deserialize)]
struct TitleReq {
    title: String,
}

/// HTTP error that renders as a JSON `{ "error": ... }` response.
struct WebError {
    status: StatusCode,
    message: String,
}

impl WebError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for WebError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

// ── Serve ─────────────────────────────────────────────────────

/// Run the web server until the process exits (or the listener fails).
#[expect(clippy::too_many_arguments)]
#[cfg_attr(not(unix), allow(unused_variables))]
pub async fn serve(
    agent: AgentSession,
    config: Config,
    soul: Soul,
    memory: MemoryStore,
    kstore: KnowledgeStore,
    skills: SkillsIndex,
    resolved: ResolvedModel,
    host: String,
    port: u16,
    no_browser: bool,
) -> Result<()> {
    let model = agent.resolved.model.clone();
    let provider = short_provider(&agent.resolved.base_url);
    let tool_count = agent.info().tool_count;

    println!();
    println!(
        "{} Enchanter web UI v{}",
        "⟡".bright_magenta().bold(),
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "  {} http://{}:{}  {}",
        "URL:".dimmed(),
        host,
        port,
        "(Ctrl+C to stop)".dimmed()
    );
    println!(
        "  {} model={} | provider={} | tools={}",
        "↳".dimmed(),
        model.bright_white(),
        provider,
        tool_count
    );
    println!(
        "{} No authentication — local development only. Do not expose publicly.",
        "Warning:".yellow()
    );
    println!();

    #[cfg(unix)]
    if !no_browser {
        let url = format!("http://{}:{}/", host, port);
        if std::env::consts::OS == "macos" {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        } else if std::env::var("DISPLAY")
            .map(|d| !d.is_empty())
            .unwrap_or(false)
        {
            let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
        }
    }

    let skills_json: Vec<Value> = skills
        .skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "category": s.category,
                "description": s.description,
            })
        })
        .collect();

    let state = Arc::new(WebState {
        config,
        soul,
        memory,
        kstore,
        skills,
        resolved,
        agent: Mutex::new(Some(agent)),
        abort: Mutex::new(None),
        snapshot: Mutex::new(Value::Null),
        resources: Mutex::new(json!({ "skills": skills_json, "mcp": [] })),
    });

    let app = router(state);
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("{} Serving on http://{}:{}", "✓".green(), host, port);
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/{id}", get(api_history))
        .route("/api/resources", get(api_resources))
        .route("/api/session/title", post(api_session_title))
        .route("/api/chat", post(api_chat))
        .route("/api/chat/retry", post(api_chat_retry))
        .route("/api/chat/resume", post(api_chat_resume))
        .route("/api/models", get(api_models))
        .route("/api/model", post(api_model))
        .route("/api/undo", post(api_undo))
        .route("/api/clear", post(api_clear))
        .route("/api/stop", post(api_stop))
        .with_state(state)
}

fn short_provider(base_url: &str) -> String {
    base_url
        .trim_end_matches('/')
        .replace("https://api.openai.com/v1", "openai")
        .replace("http://localhost:11434/v1", "ollama")
        .replace("http://127.0.0.1:11434/v1", "ollama")
        .replace("https://openrouter.ai/api/v1", "openrouter")
        .replace("https://api.groq.com/openai/v1", "groq")
}

// ── Pages & status ────────────────────────────────────────────

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("web/index.html"),
    )
}

async fn api_status(State(state): State<Arc<WebState>>) -> Json<Value> {
    match state.agent.try_lock() {
        Ok(guard) => match guard.as_ref() {
            Some(agent) => {
                let v = status_json(agent);
                *state.snapshot.lock().await = v.clone();
                Json(v)
            }
            None => Json(snapshot_or_idle(&state).await),
        },
        // Lock held by a streaming turn — return the last snapshot.
        Err(_) => Json(state.snapshot.lock().await.clone()),
    }
}

async fn snapshot_or_idle(state: &WebState) -> Value {
    let saved = state.snapshot.lock().await.clone();
    if !saved.is_null() {
        return saved;
    }
    // Nothing has run yet and the agent is in transition.
    json!({
        "model": state.resolved.model,
        "base_url": state.resolved.base_url,
        "api_key_set": state.resolved.api_key.is_some(),
        "session_id": null,
        "session_title": null,
        "estimated_context_tokens": 0,
        "context_budget": null,
        "token_usage": { "prompt": 0, "completion": 0, "total": 0 },
        "tool_count": 0,
        "mcp_tool_count": 0,
        "skill_count": state.skills.skills.len(),
        "max_turns": state.config.max_turns(),
        "soft_limit": state.config.soft_limit(),
        "busy": true,
    })
}

#[expect(clippy::too_many_arguments)]
fn status_value(
    model: &str,
    base_url: &str,
    api_key_set: bool,
    session_id: &str,
    session_title: Option<&str>,
    estimated_context_tokens: u64,
    context_budget: Option<u64>,
    usage: TokenUsage,
    tool_count: usize,
    mcp_tool_count: usize,
    skill_count: usize,
    max_turns: Option<u32>,
    soft_limit: Option<u32>,
) -> Value {
    json!({
        "model": model,
        "base_url": base_url,
        "api_key_set": api_key_set,
        "session_id": session_id,
        "session_title": session_title,
        "estimated_context_tokens": estimated_context_tokens,
        "context_budget": context_budget,
        "token_usage": {
            "prompt": usage.prompt_tokens,
            "completion": usage.completion_tokens,
            "total": usage.total_tokens,
        },
        "tool_count": tool_count,
        "mcp_tool_count": mcp_tool_count,
        "skill_count": skill_count,
        "max_turns": max_turns,
        "soft_limit": soft_limit,
    })
}

fn status_json(agent: &AgentSession) -> Value {
    let info = agent.info();
    status_value(
        &info.model,
        &info.base_url,
        info.api_key_set,
        &info.session_id,
        info.session_title.as_deref(),
        agent.estimated_context_tokens(),
        agent.context_budget(),
        agent.token_usage(),
        info.tool_count,
        info.mcp_tool_count,
        info.skill_count,
        info.max_turns,
        info.soft_limit,
    )
}

// ── Session endpoints ─────────────────────────────────────────

async fn api_sessions() -> Result<Json<Value>, WebError> {
    let sessions = Session::list_all().map_err(WebError::from)?;
    let list: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "started_at": s.started_at,
                "model": s.model,
                "title": s.title,
                "message_count": s.message_count,
                "file_size": s.file_size,
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": list })))
}

async fn api_history(Path(id): Path<String>) -> Result<Json<Value>, WebError> {
    let entries = Session::load(&id).map_err(|e| {
        WebError::new(
            StatusCode::NOT_FOUND,
            format!("session '{}' not found: {}", id, e),
        )
    })?;
    Ok(Json(json!({
        "id": id,
        "messages": history_from_entries(&entries),
    })))
}

async fn api_resources(State(state): State<Arc<WebState>>) -> Json<Value> {
    let skills_json: Vec<Value> = state
        .skills
        .skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "category": s.category,
                "description": s.description,
            })
        })
        .collect();

    let mcp = match state.agent.try_lock() {
        Ok(guard) => match guard.as_ref() {
            Some(agent) => {
                let mcp_json: Vec<Value> = agent
                    .mcp
                    .summaries()
                    .into_iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "transport": s.transport,
                            "tools": s.tools.iter().map(|t| {
                                json!({
                                    "name": t.name,
                                    "description": t.description,
                                })
                            }).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let v = json!({ "skills": skills_json, "mcp": mcp_json });
                *state.resources.lock().await = v.clone();
                v
            }
            None => state.resources.lock().await.clone(),
        },
        // Lock held by a streaming turn — return the cached snapshot.
        Err(_) => state.resources.lock().await.clone(),
    };
    Json(mcp)
}

async fn api_session_title(
    State(state): State<Arc<WebState>>,
    Json(req): Json<TitleReq>,
) -> Result<Json<Value>, WebError> {
    let mut guard = match state.agent.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            return Err(WebError::new(
                StatusCode::CONFLICT,
                "agent busy — cannot rename while a turn is in flight",
            ));
        }
    };
    let agent = guard
        .as_mut()
        .ok_or_else(|| WebError::new(StatusCode::CONFLICT, "agent unavailable"))?;
    agent
        .session
        .set_title(&req.title)
        .map_err(WebError::from)?;
    let status = status_json(agent);
    *state.snapshot.lock().await = status.clone();
    Ok(Json(json!({ "ok": true, "status": status })))
}

async fn api_chat_resume(
    State(state): State<Arc<WebState>>,
    Json(req): Json<ResumeReq>,
) -> Result<Json<Value>, WebError> {
    let mut agent = AgentSession::resume(
        state.config.clone(),
        state.soul.clone(),
        state.memory.clone(),
        state.kstore.clone(),
        state.skills.clone(),
        state.resolved.clone(),
        SessionOptions {
            no_stream: false,
            no_tools: false,
            system_override: None,
            session_name: None,
        },
        &req.session_id,
    )
    .map_err(|e| {
        WebError::new(
            StatusCode::NOT_FOUND,
            format!("session '{}' not found: {}", req.session_id, e),
        )
    })?;
    agent.start_mcp().await;
    let history = history_from_messages(&agent.messages);
    let status = status_json(&agent);
    {
        let mut guard = state.agent.lock().await;
        if guard.is_some() {
            return Err(WebError::new(
                StatusCode::CONFLICT,
                "a conversation is already in flight",
            ));
        }
        *guard = Some(agent);
        *state.snapshot.lock().await = status.clone();
    }
    Ok(Json(
        json!({ "ok": true, "status": status, "history": history }),
    ))
}

fn history_from_entries(entries: &[SessionEntry]) -> Vec<Value> {
    entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::User { content } => Some(json!({ "role": "user", "content": content })),
            SessionEntry::Assistant { content } if !content.is_empty() => {
                Some(json!({ "role": "assistant", "content": content }))
            }
            _ => None,
        })
        .collect()
}

fn history_from_messages(messages: &[crate::api::Message]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| match m.role.as_str() {
            "user" => {
                Some(json!({ "role": "user", "content": m.content.clone().unwrap_or_default() }))
            }
            "assistant" => m
                .content
                .clone()
                .filter(|c| !c.is_empty())
                .map(|c| json!({ "role": "assistant", "content": c })),
            _ => None,
        })
        .collect()
}

// ── Model & session controls ──────────────────────────────────

async fn api_models(State(state): State<Arc<WebState>>) -> Json<Value> {
    let current = match state.agent.try_lock() {
        Ok(guard) => guard
            .as_ref()
            .map(|a| a.resolved.model.clone())
            .unwrap_or_else(|| state.resolved.model.clone()),
        Err(_) => state
            .snapshot
            .lock()
            .await
            .get("model")
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| state.resolved.model.clone()),
    };
    let mut provider_names: Vec<&String> = state.config.providers.keys().collect();
    provider_names.sort();
    let providers: Vec<Value> = provider_names
        .into_iter()
        .filter_map(|name| {
            state
                .config
                .resolve_provider(name)
                .map(|r| json!({ "name": name, "model": r.model }))
        })
        .collect();
    Json(json!({ "providers": providers, "current": current }))
}

async fn api_model(
    State(state): State<Arc<WebState>>,
    Json(req): Json<ModelReq>,
) -> Result<Json<Value>, WebError> {
    let mut guard = state.agent.lock().await;
    let agent = guard
        .as_mut()
        .ok_or_else(|| WebError::new(StatusCode::CONFLICT, "agent unavailable"))?;
    let label = agent.switch_model(&req.name).map_err(WebError::from)?;
    let status = status_json(agent);
    *state.snapshot.lock().await = status.clone();
    Ok(Json(
        json!({ "ok": true, "label": label, "status": status }),
    ))
}

async fn api_undo(State(state): State<Arc<WebState>>) -> Result<Json<Value>, WebError> {
    let mut guard = state.agent.lock().await;
    let agent = guard
        .as_mut()
        .ok_or_else(|| WebError::new(StatusCode::CONFLICT, "agent unavailable"))?;
    let undone = agent.undo();
    *state.snapshot.lock().await = status_json(agent);
    Ok(Json(json!({ "ok": true, "undone": undone })))
}

async fn api_clear(State(state): State<Arc<WebState>>) -> Result<Json<Value>, WebError> {
    let mut guard = state.agent.lock().await;
    let agent = guard
        .as_mut()
        .ok_or_else(|| WebError::new(StatusCode::CONFLICT, "agent unavailable"))?;
    agent.clear().map_err(WebError::from)?;
    let status = status_json(agent);
    *state.snapshot.lock().await = status.clone();
    Ok(Json(json!({ "ok": true, "status": status })))
}

async fn api_stop(State(state): State<Arc<WebState>>) -> Json<Value> {
    let abort = state.abort.lock().await.take();
    match abort {
        Some(tx) => {
            let _ = tx.send(());
            Json(json!({ "ok": true, "stopped": true }))
        }
        None => {
            Json(json!({ "ok": true, "stopped": false, "message": "no conversation in flight" }))
        }
    }
}

// ── Chat / SSE ────────────────────────────────────────────────

async fn api_chat(
    State(state): State<Arc<WebState>>,
    Json(req): Json<ChatReq>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, WebError> {
    let rx = spawn_turn(state, Some(req.prompt)).await?;
    Ok(Sse::new(sse_stream(rx)).keep_alive(KeepAlive::default()))
}

async fn api_chat_retry(
    State(state): State<Arc<WebState>>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, WebError> {
    let rx = spawn_turn(state, None).await?;
    Ok(Sse::new(sse_stream(rx)).keep_alive(KeepAlive::default()))
}

/// Serialize a protocol event as the payload of an SSE `data:` frame.
fn sse_data(ev: &Event) -> serde_json::Result<String> {
    serde_json::to_string(ev)
}

fn sse_stream(rx: broadcast::Receiver<Event>) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    BroadcastStream::new(rx)
        .filter_map(|item| std::future::ready(item.ok()))
        .map(|ev| Ok::<_, Infallible>(SseEvent::default().data(sse_data(&ev).unwrap_or_default())))
}

/// Take the agent out of shared state, run one turn, and stream its events.
/// The lock is moved into a background task, so it is held for the whole turn
/// (one conversation at a time, like the REPL); the session is restored when
/// the turn finishes. A fresh agent is rebuilt if the turn is aborted.
async fn spawn_turn(
    state: Arc<WebState>,
    prompt: Option<String>,
) -> Result<broadcast::Receiver<Event>, WebError> {
    let (tx, bcast_rx) = broadcast::channel(512);
    let (abort_tx, abort_rx) = oneshot::channel();

    {
        let mut busy = state.abort.lock().await;
        if busy.is_some() {
            return Err(WebError::new(
                StatusCode::CONFLICT,
                "a conversation is already in flight",
            ));
        }
        *busy = Some(abort_tx);
    }

    let state2 = state.clone();
    tokio::spawn(async move {
        let mut agent = {
            let mut guard = state2.agent.lock().await;
            guard.take().expect("agent must be Some when idle")
        };

        // Give every fresh session a meaningful title from its first message.
        if let Some(prompt) = &prompt
            && agent.session.title().is_none()
            && let Err(e) = agent
                .session
                .set_title(&crate::session::derive_title(prompt))
        {
            eprintln!("{} web: failed to set session title: {}", "⚠".yellow(), e);
        }

        let handle_and_rx = match &prompt {
            Some(prompt) => agent.chat_events_spawned(prompt),
            None => agent.retry_events_spawned(),
        };
        let (handle, mut rx) = match handle_and_rx {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("{} web: failed to start turn: {}", "✗".red(), e);
                *state2.agent.lock().await = build_agent(&state2).await.ok();
                *state2.abort.lock().await = None;
                let _ = tx.send(Event::Error {
                    message: e.to_string(),
                });
                return;
            }
        };

        let mut abort = Some(abort_rx);
        let mut stopped = false;
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ev) => {
                        // Ignore send errors: no SSE receiver means the client
                        // disconnected, but the turn still completes and the
                        // session is restored.
                        let _ = tx.send(ev);
                    }
                    None => break,
                },
                _ = async {
                    match abort.as_mut() {
                        Some(r) => r.await,
                        None => {
                            let _: () = std::future::pending().await;
                            Ok(())
                        }
                    }
                }, if abort.is_some() => {
                    stopped = true;
                    break;
                }
            }
        }

        let mut guard = state2.agent.lock().await;
        if stopped {
            handle.abort();
            eprintln!("{} web: turn stopped via /api/stop", "⟡".dimmed());
            *guard = build_agent(&state2).await.ok();
        } else {
            match handle.await {
                Ok(Ok(agent)) => *guard = Some(agent),
                Ok(Err(e)) => {
                    eprintln!("{} web agent error: {}", "✗".red(), e);
                    *guard = build_agent(&state2).await.ok();
                }
                Err(e) => {
                    eprintln!("{} web task join error: {}", "✗".red(), e);
                    *guard = build_agent(&state2).await.ok();
                }
            }
        }
        if let Some(ref a) = *guard {
            *state2.snapshot.lock().await = status_json(a);
        }
        drop(guard);
        *state2.abort.lock().await = None;
    });

    Ok(bcast_rx)
}

/// Rebuild a fresh session from the boot data (used after a stopped/aborted
/// turn, or when a turn failed to hand the session back).
async fn build_agent(state: &WebState) -> Result<AgentSession> {
    let mut agent = AgentSession::new(
        state.config.clone(),
        state.soul.clone(),
        state.memory.clone(),
        state.kstore.clone(),
        state.skills.clone(),
        state.resolved.clone(),
        SessionOptions {
            no_stream: false,
            no_tools: false,
            system_override: None,
            session_name: None,
        },
    )?;
    agent.session.append(&agent.messages[0])?;
    agent.start_mcp().await;
    Ok(agent)
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Event;

    fn sample_resolved() -> ResolvedModel {
        ResolvedModel {
            model: "test-model".into(),
            base_url: "http://localhost:11434/v1/chat/completions".into(),
            api_key: None,
            extra_headers: vec![],
            context_window: Some(16_000),
        }
    }

    fn test_state() -> WebState {
        WebState {
            config: Config::default(),
            soul: Soul {
                content: "test".into(),
                source: std::path::PathBuf::from("test"),
            },
            memory: MemoryStore::default(),
            kstore: KnowledgeStore::default(),
            skills: SkillsIndex::default(),
            resolved: sample_resolved(),
            agent: Mutex::new(None),
            abort: Mutex::new(None),
            snapshot: Mutex::new(Value::Null),
            resources: Mutex::new(json!({ "skills": [], "mcp": [] })),
        }
    }

    #[test]
    fn status_json_shape() {
        let v = status_value(
            "test-model",
            "http://localhost",
            true,
            "sess-1",
            Some("test title"),
            1234,
            Some(16_000),
            TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            8,
            2,
            3,
            Some(150),
            Some(10),
        );
        assert_eq!(v["model"], "test-model");
        assert_eq!(v["base_url"], "http://localhost");
        assert_eq!(v["api_key_set"], true);
        assert_eq!(v["session_id"], "sess-1");
        assert_eq!(v["session_title"], "test title");
        assert_eq!(v["estimated_context_tokens"], 1234);
        assert_eq!(v["context_budget"], 16_000);
        assert_eq!(v["token_usage"]["prompt"], 10);
        assert_eq!(v["token_usage"]["completion"], 5);
        assert_eq!(v["token_usage"]["total"], 15);
        assert_eq!(v["tool_count"], 8);
        assert_eq!(v["mcp_tool_count"], 2);
        assert_eq!(v["skill_count"], 3);
        assert_eq!(v["max_turns"], 150);
        assert_eq!(v["soft_limit"], 10);
        // Optional budget serializes as JSON null.
        assert_eq!(
            status_value(
                "m",
                "",
                false,
                "s",
                None,
                0,
                None,
                TokenUsage::default(),
                0,
                0,
                0,
                None,
                None
            )["context_budget"],
            Value::Null
        );
        // Optional session_title serializes as JSON null, not omitted.
        assert_eq!(
            status_value(
                "m",
                "",
                false,
                "s",
                None,
                0,
                None,
                TokenUsage::default(),
                0,
                0,
                0,
                None,
                None
            )["session_title"],
            Value::Null
        );
    }

    #[test]
    fn sse_data_frame() {
        let ev = Event::Content {
            text: "hello".into(),
        };
        let frame = format!("data: {}\n\n", sse_data(&ev).unwrap());
        assert!(frame.starts_with("data: "));
        assert!(frame.ends_with("\n\n"));
        let parsed: Event =
            serde_json::from_str(frame.trim_start_matches("data: ").trim_end()).unwrap();
        match parsed {
            Event::Content { text } => assert_eq!(text, "hello"),
            _ => panic!("expected Content event"),
        }
    }

    #[test]
    fn history_parse() {
        let entries = vec![
            SessionEntry::System {
                content: "sys".into(),
            },
            SessionEntry::User {
                content: "hello".into(),
            },
            SessionEntry::Assistant {
                content: "hi".into(),
            },
            SessionEntry::ToolCall {
                id: "c1".into(),
                name: "exec_command".into(),
                arguments: "{}".into(),
            },
            SessionEntry::ToolResult {
                id: "c1".into(),
                content: "ok".into(),
            },
        ];
        let msgs = history_from_entries(&entries);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "hello");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "hi");
    }

    #[test]
    fn status_budget_null_when_unknown() {
        let v = status_value(
            "m",
            "",
            false,
            "s",
            None,
            0,
            None,
            TokenUsage::default(),
            0,
            0,
            0,
            None,
            None,
        );
        assert_eq!(v["context_budget"], Value::Null);
        assert_eq!(v["max_turns"], Value::Null);
        assert_eq!(v["soft_limit"], Value::Null);
    }

    #[tokio::test]
    async fn router_serves_index_and_models() {
        use tower::ServiceExt;

        let app = router(Arc::new(test_state()));
        let resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Enchanter"));
        assert!(html.contains("/api/chat"));

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/models")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
