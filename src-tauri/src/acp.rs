//! The ACP session runtime.
//!
//! One live session = one OS thread running a current-thread Tokio runtime that
//! owns the connection to one agent process. The rest of the application talks
//! to it through a command channel, so no Tauri/UI type leaks into the
//! protocol layer and a hung agent cannot block the UI.
//!
//! This module uses the official Rust SDK (`agent-client-protocol`) for the
//! stable v1 protocol. LJ Hub does not implement its own protocol stack.

use crate::events::{EventCallback, EventPipeline};
use crate::model::{
    AgentCapabilitiesReport, AgentStatus, MessageBody, PermissionChoice, SessionEvent,
};
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, ContentChunk, CurrentModeUpdate,
    FileSystemCapabilities, Implementation, InitializeRequest, LoadSessionRequest,
    NewSessionRequest, Plan, PromptRequest, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, TextContent, ToolCall,
    ToolCallLocation, ToolCallStatus, ToolCallUpdate, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const CLIENT_NAME: &str = "lj-hub";
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A permission request that is waiting for the user. The UI resolves it by
/// id; until then the agent is genuinely blocked, which is the point.
#[derive(Clone, Default)]
pub struct PermissionRegistry {
    inner: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Option<String>>>>>,
}

impl PermissionRegistry {
    pub fn register(&self, id: String, tx: tokio::sync::oneshot::Sender<Option<String>>) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id, tx);
        }
    }

    /// Resolve a pending request. Returns false when the request is unknown or
    /// was already resolved, so a duplicate click cannot grant anything twice.
    pub fn resolve(&self, id: &str, outcome: Option<String>) -> bool {
        let sender = self.inner.lock().ok().and_then(|mut map| map.remove(id));
        match sender {
            Some(tx) => tx.send(outcome).is_ok(),
            None => false,
        }
    }

    pub fn pending_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub task_id: String,
    pub agent_id: String,
    pub project_root: PathBuf,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub extra_env: Vec<(String, String)>,
    /// The agent's own session id to try to restore. Only used when the agent
    /// supports `session/load`.
    pub resume_native: Option<String>,
}

#[derive(Debug)]
pub struct ReadyInfo {
    pub native_session_id: String,
    pub native_session_restored: bool,
    pub capabilities: AgentCapabilitiesReport,
    pub agent_info: Option<String>,
    pub auth_methods: Vec<String>,
    pub detail: String,
}

#[derive(Debug)]
pub struct PromptOutcome {
    pub stop_reason: String,
    pub error: Option<String>,
}

enum Command {
    Prompt {
        blocks: Vec<ContentBlock>,
        reply: tokio::sync::oneshot::Sender<PromptOutcome>,
    },
    Cancel,
    Shutdown,
}

pub struct LiveSession {
    pub task_id: String,
    pub agent_id: String,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<Command>,
    native_session_id: Arc<Mutex<Option<String>>>,
    pub alive: Arc<AtomicBool>,
    pipeline: Arc<EventPipeline>,
    permissions: PermissionRegistry,
}

impl LiveSession {
    pub fn native_session_id(&self) -> Option<String> {
        self.native_session_id.lock().ok().and_then(|g| g.clone())
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub async fn prompt(&self, blocks: Vec<ContentBlock>) -> Result<PromptOutcome, String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(Command::Prompt {
                blocks,
                reply: reply_tx,
            })
            .map_err(|_| "会话已结束".to_string())?;
        reply_rx.await.map_err(|_| "会话在响应前结束".to_string())
    }

    /// Send a plain text prompt. Convenience wrapper used by the app and tests.
    pub async fn prompt_text(&self, text: &str) -> Result<PromptOutcome, String> {
        self.prompt(vec![ContentBlock::Text(TextContent::new(text))])
            .await
    }

    /// Answer a pending permission request exactly like the UI does. A missing
    /// option id means "not approved".
    pub fn resolve_permission(&self, request_id: &str, option_id: Option<String>) -> bool {
        self.permissions.resolve(request_id, option_id)
    }

    pub fn cancel(&self) {
        let _ = self.cmd_tx.send(Command::Cancel);
    }

    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }

    /// Stop the pipeline worker. Safe to call repeatedly.
    pub fn close_pipeline(&self) {
        self.pipeline.close();
    }
}

fn text_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(t) => t.text.clone(),
        ContentBlock::Image(i) => format!("[图片 {}]", i.mime_type),
        ContentBlock::Audio(a) => format!("[音频 {}]", a.mime_type),
        ContentBlock::ResourceLink(l) => format!("[引用 {}]", l.uri),
        ContentBlock::Resource(_) => "[嵌入资源]".to_string(),
        _ => "[未知内容]".to_string(),
    }
}

fn status_str(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "unknown",
    }
}

fn locations_str(locations: &[ToolCallLocation]) -> Vec<String> {
    locations
        .iter()
        .map(|l| match l.line {
            Some(line) => format!("{}:{}", l.path.display(), line),
            None => l.path.display().to_string(),
        })
        .collect()
}

fn tool_body_from_call(call: &ToolCall) -> MessageBody {
    MessageBody::Tool {
        tool_call_id: call.tool_call_id.to_string(),
        title: call.title.clone(),
        tool_kind: format!("{:?}", call.kind),
        status: status_str(call.status).to_string(),
        locations: locations_str(&call.locations),
        raw_input: call.raw_input.as_ref().map(|v| v.to_string()),
        raw_output: call.raw_output.as_ref().map(|v| v.to_string()),
    }
}

fn tool_body_from_update(update: &ToolCallUpdate, previous: Option<&MessageBody>) -> MessageBody {
    let (mut title, mut tool_kind, mut status, mut locations, mut raw_input, mut raw_output) = (
        String::new(),
        String::new(),
        String::new(),
        Vec::new(),
        None,
        None,
    );
    if let MessageBody::Tool {
        title: t,
        tool_kind: k,
        status: s,
        locations: l,
        raw_input: ri,
        raw_output: ro,
        ..
    } = previous.unwrap_or(&MessageBody::System {
        text: String::new(),
    }) {
        title = t.clone();
        tool_kind = k.clone();
        status = s.clone();
        locations = l.clone();
        raw_input = ri.clone();
        raw_output = ro.clone();
    }
    if let Some(t) = &update.fields.title {
        title = t.clone();
    }
    if let Some(k) = &update.fields.kind {
        tool_kind = format!("{k:?}");
    }
    if let Some(s) = &update.fields.status {
        status = status_str(*s).to_string();
    }
    if let Some(l) = &update.fields.locations {
        locations = locations_str(l);
    }
    if let Some(v) = &update.fields.raw_input {
        raw_input = Some(v.to_string());
    }
    if let Some(v) = &update.fields.raw_output {
        raw_output = Some(v.to_string());
    }
    MessageBody::Tool {
        tool_call_id: update.tool_call_id.to_string(),
        title,
        tool_kind,
        status,
        locations,
        raw_input,
        raw_output,
    }
}

/// Keep the previous state of each tool call so incremental updates can be
/// merged instead of losing the title/locations already reported.
#[derive(Default)]
struct ToolCallBook {
    inner: Mutex<HashMap<String, MessageBody>>,
}

impl ToolCallBook {
    fn put(&self, body: MessageBody) {
        if let MessageBody::Tool { tool_call_id, .. } = &body {
            if let Ok(mut map) = self.inner.lock() {
                map.insert(tool_call_id.clone(), body);
            }
        }
    }
    fn get(&self, id: &str) -> Option<MessageBody> {
        self.inner.lock().ok().and_then(|m| m.get(id).cloned())
    }
}

/// Resolve a path and refuse anything outside the project directory.
pub fn sandbox_path(root: &Path, requested: &Path) -> Result<PathBuf, String> {
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("项目目录不可访问：{e}"))?;
    let probe = if candidate.exists() {
        candidate.canonicalize().map_err(|e| e.to_string())?
    } else {
        let parent = candidate.parent().unwrap_or(root);
        parent.canonicalize().map_err(|e| e.to_string())?.join(
            candidate
                .file_name()
                .map(|f| f.to_os_string())
                .unwrap_or_default(),
        )
    };
    if !probe.starts_with(&canon_root) {
        return Err(format!(
            "拒绝访问项目目录之外的路径：{}",
            candidate.display()
        ));
    }
    // Return the canonical form so callers always operate on the same, fully
    // resolved path that was checked here.
    Ok(probe)
}

fn map_update(update: &SessionUpdate, book: &ToolCallBook) -> Option<SessionEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(ContentChunk { content, .. }) => {
            Some(SessionEvent::Chunk {
                stream: "agent".into(),
                text: text_of(content),
            })
        }
        SessionUpdate::AgentThoughtChunk(ContentChunk { content, .. }) => {
            Some(SessionEvent::Chunk {
                stream: "thought".into(),
                text: text_of(content),
            })
        }
        SessionUpdate::UserMessageChunk(ContentChunk { content, .. }) => {
            Some(SessionEvent::Chunk {
                stream: "user".into(),
                text: text_of(content),
            })
        }
        SessionUpdate::ToolCall(call) => {
            let body = tool_body_from_call(call);
            book.put(body.clone());
            Some(SessionEvent::ToolCall { tool: body })
        }
        SessionUpdate::ToolCallUpdate(update) => {
            let previous = book.get(&update.tool_call_id.to_string());
            let body = tool_body_from_update(update, previous.as_ref());
            book.put(body.clone());
            Some(SessionEvent::ToolCall { tool: body })
        }
        SessionUpdate::Plan(Plan { entries, .. }) => Some(SessionEvent::Plan {
            entries: entries
                .iter()
                .map(|e| format!("[{}] {}", e.status_short(), e.content))
                .collect(),
        }),
        SessionUpdate::CurrentModeUpdate(CurrentModeUpdate {
            current_mode_id, ..
        }) => Some(SessionEvent::Mode {
            mode: current_mode_id.to_string(),
        }),
        _ => None,
    }
}

trait PlanEntryExt {
    fn status_short(&self) -> &'static str;
}

impl PlanEntryExt for agent_client_protocol::schema::v1::PlanEntry {
    fn status_short(&self) -> &'static str {
        use agent_client_protocol::schema::v1::PlanEntryStatus::*;
        match self.status {
            Pending => " ",
            InProgress => "~",
            Completed => "x",
            _ => "?",
        }
    }
}

/// Start a live ACP session. Resolves once the handshake finished, so the
/// caller knows whether the agent really answered — a session is never
/// reported as running just because a process was spawned.
pub async fn spawn_session(
    spec: SessionSpec,
    sink: EventCallback,
    perms: PermissionRegistry,
) -> Result<LiveSession, String> {
    let task_id = spec.task_id.clone();
    let agent_id = spec.agent_id.clone();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<ReadyInfo, String>>();
    let native_session_id = Arc::new(Mutex::new(None));
    let native_slot = native_session_id.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let alive_thread = alive.clone();
    let pipeline = Arc::new(EventPipeline::new(sink));
    let pipeline_for_thread = pipeline.clone();
    let permissions = perms.clone();

    std::thread::Builder::new()
        .name(format!("acp-{task_id}"))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("无法创建会话运行时：{e}")));
                    alive_thread.store(false, Ordering::SeqCst);
                    return;
                }
            };
            let result = runtime.block_on(drive(
                spec,
                pipeline_for_thread,
                perms,
                cmd_rx,
                ready_tx,
                native_slot,
            ));
            if let Err(e) = result {
                tracing::warn!("ACP 会话结束：{e}");
            }
            alive_thread.store(false, Ordering::SeqCst);
        })
        .map_err(|e| format!("无法启动会话线程：{e}"))?;

    match ready_rx.await {
        Ok(Ok(info)) => {
            *native_session_id.lock().unwrap() = Some(info.native_session_id.clone());
            Ok(LiveSession {
                task_id,
                agent_id,
                cmd_tx,
                native_session_id,
                alive,
                pipeline,
                permissions,
            })
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("会话线程在握手完成前退出".to_string()),
    }
}

async fn drive(
    spec: SessionSpec,
    pipeline: Arc<EventPipeline>,
    perms: PermissionRegistry,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Command>,
    ready_tx: tokio::sync::oneshot::Sender<Result<ReadyInfo, String>>,
    native_slot: Arc<Mutex<Option<String>>>,
) -> Result<(), String> {
    let config = AcpAgentConfig::new(spec.executable.clone())
        .args(spec.args.clone())
        .envs(spec.extra_env.clone());
    let agent = AcpAgent::new(config);

    let task_id = spec.task_id.clone();
    let root = spec.project_root.clone();
    let root_for_fs = root.clone();
    let book = Arc::new(ToolCallBook::default());
    let book_notif = book.clone();
    let pipeline_notif = pipeline.clone();
    let ready_tx = Arc::new(Mutex::new(Some(ready_tx)));

    let pipeline_for_perm = pipeline.clone();

    let resume_native = spec.resume_native.clone();
    let resume_for_main = resume_native.clone();
    let ready_for_main = ready_tx.clone();
    let native_for_main = native_slot.clone();
    let pipeline_main = pipeline_for_perm.clone();

    let build = Client
        .builder()
        .on_receive_notification(
            move |notification: SessionNotification, _cx| {
                let book = book_notif.clone();
                let pipeline = pipeline_notif.clone();
                async move {
                    if let Some(event) = map_update(&notification.update, &book) {
                        pipeline.emit(event);
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let perms = perms.clone();
                let pipeline = pipeline_for_perm.clone();
                let task_id = task_id.clone();
                move |request: RequestPermissionRequest,
                      responder: agent_client_protocol::Responder<RequestPermissionResponse>,
                      _cx| {
                    let perms = perms.clone();
                    let pipeline = pipeline.clone();
                    let task_id = task_id.clone();
                    async move {
                        let request_id = format!("perm-{}", uuid::Uuid::new_v4());
                        let options: Vec<PermissionChoice> = request
                            .options
                            .iter()
                            .map(|o| PermissionChoice {
                                option_id: o.option_id.to_string(),
                                name: o.name.clone(),
                                kind: format!("{:?}", o.kind).to_lowercase(),
                            })
                            .collect();
                        let title = request
                            .tool_call
                            .fields
                            .title
                            .clone()
                            .unwrap_or_else(|| request.tool_call.tool_call_id.to_string());
                        let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
                        perms.register(request_id.clone(), tx);
                        let _ = task_id;
                        pipeline.emit(SessionEvent::PermissionRequest {
                            request_id: request_id.clone(),
                            title,
                            options,
                        });
                        tokio::spawn(async move {
                            let answer =
                                tokio::time::timeout(std::time::Duration::from_secs(600), rx).await;
                            let outcome = match answer {
                                Ok(Ok(Some(option_id))) => RequestPermissionOutcome::Selected(
                                    SelectedPermissionOutcome::new(option_id),
                                ),
                                // Timeout, cancel or an unknown request id all
                                // mean "no approval was granted".
                                _ => RequestPermissionOutcome::Cancelled,
                            };
                            let response: RequestPermissionResponse =
                                RequestPermissionResponse::new(outcome);
                            let _ = responder.respond(response);
                        });
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let root = root_for_fs.clone();
                move |request: ReadTextFileRequest,
                      responder: agent_client_protocol::Responder<ReadTextFileResponse>,
                      _cx| {
                    let root = root.clone();
                    async move {
                        let result = (|| -> Result<String, String> {
                            let path = sandbox_path(&root, &request.path)?;
                            let content = std::fs::read_to_string(&path)
                                .map_err(|e| format!("读取失败 {}: {e}", path.display()))?;
                            Ok(apply_line_window(content, request.line, request.limit))
                        })();
                        match result {
                            Ok(content) => {
                                let _ = responder.respond(ReadTextFileResponse::new(content));
                            }
                            Err(message) => {
                                let _ = responder.respond_with_error(
                                    agent_client_protocol::Error::invalid_params().data(message),
                                );
                            }
                        }
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let root = root_for_fs.clone();
                move |request: WriteTextFileRequest,
                      responder: agent_client_protocol::Responder<WriteTextFileResponse>,
                      _cx| {
                    let root = root.clone();
                    async move {
                        let result = (|| -> Result<(), String> {
                            let path = sandbox_path(&root, &request.path)?;
                            if let Some(parent) = path.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                            }
                            std::fs::write(&path, request.content.as_bytes())
                                .map_err(|e| format!("写入失败 {}: {e}", path.display()))
                        })();
                        match result {
                            Ok(()) => {
                                let _ = responder.respond(WriteTextFileResponse::new());
                            }
                            Err(message) => {
                                let _ = responder.respond_with_error(
                                    agent_client_protocol::Error::invalid_params().data(message),
                                );
                            }
                        }
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        );

    let outcome = build
        .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
            let init = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(
                            ClientCapabilities::new()
                                .fs(
                                    FileSystemCapabilities::new()
                                        .read_text_file(true)
                                        .write_text_file(true),
                                )
                                .terminal(false),
                        )
                        .client_info(Implementation::new(CLIENT_NAME, CLIENT_VERSION)),
                )
                .block_task()
                .await?;

            let caps = capabilities_of(&init.agent_capabilities);
            let agent_info = init
                .agent_info
                .as_ref()
                .map(|i| format!("{} {}", i.name, i.version));
            let auth_methods: Vec<String> = init
                .auth_methods
                .iter()
                .map(|m| match m {
                    agent_client_protocol::schema::v1::AuthMethod::Agent(a) => a.id.to_string(),
                    agent_client_protocol::schema::v1::AuthMethod::Terminal(t) => t.id.to_string(),
                    _ => "unknown".to_string(),
                })
                .collect();

            // Restore the agent's own session only when the agent advertises
            // support and we have an id. Otherwise a fresh session is created
            // and the UI says so honestly.
            let mut restored = false;
            let mut restore_detail = String::new();
            let mut session_id = None;
            if let Some(native) = resume_for_main.clone() {
                if caps.load_session {
                    match connection
                        .send_request(LoadSessionRequest::new(native.clone(), root.clone()))
                        .block_task()
                        .await
                    {
                        Ok(_) => {
                            restored = true;
                            restore_detail = format!("已通过 session/load 恢复原生会话 {native}");
                            session_id = Some(native);
                        }
                        Err(e) => {
                            restore_detail = format!(
                                "原生会话 {native} 恢复失败（{e}）；已改为新会话，历史仍可在 LJ Hub 中阅读"
                            );
                        }
                    }
                } else {
                    restore_detail =
                        format!("该 Agent 未声明 session/load 能力，无法恢复原生会话 {native}");
                }
            }

            let session_id = match session_id {
                Some(id) => id,
                None => {
                    let response = connection
                        .send_request(NewSessionRequest::new(root.clone()))
                        .block_task()
                        .await?;
                    response.session_id.to_string()
                }
            };

            *native_for_main.lock().unwrap() = Some(session_id.clone());
            pipeline_main.emit(SessionEvent::NativeSession {
                native_session_id: session_id.clone(),
                restored,
                detail: restore_detail.clone(),
            });

            let info = ReadyInfo {
                native_session_id: session_id.clone(),
                native_session_restored: restored,
                capabilities: caps,
                agent_info,
                auth_methods,
                detail: restore_detail,
            };
            if let Some(tx) = ready_for_main.lock().unwrap().take() {
                let _ = tx.send(Ok(info));
            }

            // A prompt may take a long time, so it is driven by its own task.
            // The command loop must stay responsive: otherwise `cancel` would
            // never be processed while a turn is running.
            let busy = Arc::new(AtomicBool::new(false));
            while let Some(command) = cmd_rx.recv().await {
                match command {
                    Command::Prompt { blocks, reply } => {
                        if busy.swap(true, Ordering::SeqCst) {
                            let _ = reply.send(PromptOutcome {
                                stop_reason: "busy".into(),
                                error: Some("已有一个回合正在进行，请先取消或等待完成".to_string()),
                            });
                            continue;
                        }
                        let connection = connection.clone();
                        let session_id = session_id.clone();
                        let pipeline = pipeline_main.clone();
                        let busy_flag = busy.clone();
                        tokio::spawn(async move {
                            let result = connection
                                .send_request(PromptRequest::new(session_id, blocks))
                                .block_task()
                                .await;
                            match result {
                                Ok(response) => {
                                    let stop_reason = stop_reason_str(&response.stop_reason);
                                    pipeline.emit(SessionEvent::PromptDone {
                                        stop_reason: stop_reason.clone(),
                                    });
                                    let _ = reply.send(PromptOutcome {
                                        stop_reason,
                                        error: None,
                                    });
                                }
                                Err(e) => {
                                    let message = e.to_string();
                                    pipeline
                                        .emit(SessionEvent::Error { message: message.clone() });
                                    let _ = reply.send(PromptOutcome {
                                        stop_reason: "error".into(),
                                        error: Some(message),
                                    });
                                }
                            }
                            busy_flag.store(false, Ordering::SeqCst);
                        });
                    }
                    Command::Cancel => {
                        let _ = connection.send_notification(CancelNotification::new(
                            session_id.clone(),
                        ));
                        pipeline_main.emit(SessionEvent::Status {
                            status: "cancelling".into(),
                            message: "已请求取消当前回合".into(),
                        });
                    }
                    Command::Shutdown => break,
                }
            }
            Ok::<(), agent_client_protocol::Error>(())
        })
        .await;

    // Any permission request still waiting must not grant anything.
    for id in perms.pending_ids() {
        perms.resolve(&id, None);
    }
    pipeline.close();

    if let Err(e) = outcome {
        let message = format!("Agent 会话中断：{e}");
        if let Some(tx) = ready_tx.lock().unwrap().take() {
            let _ = tx.send(Err(message.clone()));
        }
        return Err(message);
    }
    Ok(())
}

/// Canonical wire spelling of a stop reason ("end_turn", "cancelled", ...).
/// Falls back to the enum name only if the value cannot be serialized.
pub fn stop_reason_str(reason: &agent_client_protocol::schema::v1::StopReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| format!("{reason:?}"))
}

fn apply_line_window(content: String, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content;
    }
    let start = line.unwrap_or(1).max(1) as usize - 1;
    let lines: Vec<&str> = content.lines().collect();
    let end = match limit {
        Some(l) => (start + l as usize).min(lines.len()),
        None => lines.len(),
    };
    if start >= lines.len() {
        return String::new();
    }
    lines[start..end].join("\n")
}

pub fn capabilities_of(
    caps: &agent_client_protocol::schema::v1::AgentCapabilities,
) -> AgentCapabilitiesReport {
    AgentCapabilitiesReport {
        load_session: caps.load_session,
        resume_session: caps.session_capabilities.resume.is_some(),
        list_sessions: caps.session_capabilities.list.is_some(),
        prompt_image: caps.prompt_capabilities.image,
        prompt_audio: caps.prompt_capabilities.audio,
        prompt_embedded_context: caps.prompt_capabilities.embedded_context,
        // LJ Hub v0.1 does not advertise client-side terminal support, so no
        // agent can use it. Reported as false rather than guessed.
        terminal: false,
    }
}

/// Result of a real ACP handshake, used by the "connection test" button.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub status: AgentStatus,
    pub message: String,
    pub agent_info: Option<String>,
    pub auth_methods: Vec<String>,
    pub capabilities: AgentCapabilitiesReport,
}

/// Start the agent, run `initialize`, and shut it down again. This is the only
/// thing allowed to move an agent from NOT_TESTED to AVAILABLE.
pub async fn probe(
    executable: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    timeout: std::time::Duration,
) -> ProbeOutcome {
    let config = AcpAgentConfig::new(executable.clone()).args(args.clone());
    let agent = AcpAgent::new(config);
    let work = async move {
        Client
            .builder()
            .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                let init = connection
                    .send_request(
                        InitializeRequest::new(ProtocolVersion::V1)
                            .client_capabilities(
                                ClientCapabilities::new()
                                    .fs(FileSystemCapabilities::new()
                                        .read_text_file(true)
                                        .write_text_file(true))
                                    .terminal(false),
                            )
                            .client_info(Implementation::new(CLIENT_NAME, CLIENT_VERSION)),
                    )
                    .block_task()
                    .await?;
                Ok::<_, agent_client_protocol::Error>(init)
            })
            .await
    };

    match tokio::time::timeout(timeout, work).await {
        Err(_) => ProbeOutcome {
            status: AgentStatus::Error,
            message: format!("ACP 握手超时（{} 秒）", timeout.as_secs()),
            agent_info: None,
            auth_methods: Vec::new(),
            capabilities: AgentCapabilitiesReport::default(),
        },
        Ok(Err(e)) => ProbeOutcome {
            status: AgentStatus::Error,
            message: format!("ACP 握手失败：{e}"),
            agent_info: None,
            auth_methods: Vec::new(),
            capabilities: AgentCapabilitiesReport::default(),
        },
        Ok(Ok(init)) => {
            let capabilities = capabilities_of(&init.agent_capabilities);
            let agent_info = init
                .agent_info
                .as_ref()
                .map(|i| format!("{} {}", i.name, i.version));
            let auth_methods: Vec<String> = init
                .auth_methods
                .iter()
                .map(|m| match m {
                    agent_client_protocol::schema::v1::AuthMethod::Agent(a) => a.id.to_string(),
                    agent_client_protocol::schema::v1::AuthMethod::Terminal(t) => t.id.to_string(),
                    _ => "unknown".to_string(),
                })
                .collect();
            if auth_methods.is_empty() {
                ProbeOutcome {
                    status: AgentStatus::Available,
                    message: format!("ACP 握手成功（cwd: {}）", cwd.display()),
                    agent_info,
                    auth_methods,
                    capabilities,
                }
            } else {
                ProbeOutcome {
                    status: AgentStatus::AuthRequired,
                    message: format!(
                        "Agent 声明需要认证：{}。请在原 CLI 中完成登录后重试。",
                        auth_methods.join(", ")
                    ),
                    agent_info,
                    auth_methods,
                    capabilities,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sandbox_refuses_paths_outside_project() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
        let inside = sandbox_path(root, Path::new("src/main.rs")).unwrap();
        // macOS resolves /var to /private/var, so compare canonical forms.
        assert!(inside
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()));
        let outside = sandbox_path(root, Path::new("/etc/hosts"));
        assert!(outside.is_err(), "读取项目外文件必须被拒绝");
    }

    #[test]
    fn sandbox_allows_absolute_path_inside_project() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hi").unwrap();
        assert!(sandbox_path(dir.path(), &file).is_ok());
    }

    #[test]
    fn line_window_is_applied() {
        let content = "1\n2\n3\n4\n5".to_string();
        assert_eq!(apply_line_window(content.clone(), Some(2), Some(2)), "2\n3");
        assert_eq!(apply_line_window(content.clone(), None, None), content);
        assert_eq!(apply_line_window(content, Some(9), Some(2)), "");
    }

    #[test]
    fn permission_registry_does_not_grant_twice() {
        let registry = PermissionRegistry::default();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        registry.register("p1".into(), tx);
        assert!(registry.resolve("p1", Some("allow".into())));
        assert!(!registry.resolve("p1", Some("allow".into())));
        assert_eq!(rx.try_recv().unwrap(), Some("allow".to_string()));
    }
}
