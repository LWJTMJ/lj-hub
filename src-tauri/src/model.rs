//! Shared data types for LJ Hub.
//!
//! These types are the persisted (JSON) and IPC (Tauri command) contract.
//! Keeping them in one module makes the handoff / persistence format auditable.

use serde::{Deserialize, Serialize};

/// Current UTC timestamp in RFC 3339 form. One helper so every persisted
/// timestamp uses the same format.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Short random identifier used for local records.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// Real support status of an agent integration.
///
/// `NotTested` is the honest default for anything that has not been exercised
/// on this machine: finding an executable on `PATH` is *not* evidence that the
/// agent can be driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentStatus {
    Available,
    NotInstalled,
    AuthRequired,
    ConfigRequired,
    Unsupported,
    Error,
    NotTested,
}

/// How an agent is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// A local process speaking ACP over stdio.
    AcpStdio,
    /// A remote HTTP API (used by the WorkBuddy Open API adapter).
    OpenApiHttp,
}

/// A resolved, runnable agent integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReport {
    pub id: String,
    pub display_name: String,
    pub vendor: String,
    pub transport: TransportKind,
    pub status: AgentStatus,
    /// Resolved executable path, when one was found.
    pub executable: Option<String>,
    /// Version string as reported by the executable itself.
    pub version: Option<String>,
    /// Arguments used to start the ACP server.
    pub acp_args: Vec<String>,
    /// Whether the executable path comes from the user override.
    pub overridden: bool,
    /// Human readable explanation for the current status.
    pub message: String,
    /// Auth method ids advertised by the agent during the handshake.
    pub auth_methods: Vec<String>,
    /// Agent name/version as advertised by the agent during the handshake.
    pub agent_info: Option<String>,
    /// Feature flags confirmed by a real handshake.
    pub capabilities: AgentCapabilitiesReport,
    pub doc_url: String,
    pub notes: String,
    pub last_tested_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilitiesReport {
    pub load_session: bool,
    pub resume_session: bool,
    pub list_sessions: bool,
    pub prompt_image: bool,
    pub prompt_audio: bool,
    pub prompt_embedded_context: bool,
    pub terminal: bool,
}

/// A user supplied override for an agent executable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOverride {
    pub executable: Option<String>,
    pub args: Vec<String>,
    /// Explicit opt-in for the pinned official npm adapter (Codex). Off by
    /// default: LJ Hub never runs an unpinned `npx latest`.
    #[serde(default)]
    pub allow_npm_adapter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Absolute path to the project directory chosen by the user.
    pub path: String,
    /// Absolute path to the Git work tree root, when the directory is a repo.
    pub git_root: Option<String>,
    pub git_remote: Option<String>,
    pub created_at: String,
    pub last_opened_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFileChange {
    /// Two letter porcelain status code, e.g. ` M`, `??`, `A `.
    pub status: String,
    pub path: String,
    pub staged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitState {
    pub is_repo: bool,
    pub root: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub dirty: bool,
    pub changes: Vec<GitFileChange>,
    pub untracked: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskStatus {
    /// A session the user is working in.
    Active,
    /// Handoff material has been prepared by the sending agent.
    HandoffPrepared,
    /// The receiving agent verified the handoff and accepted it.
    HandoffAccepted,
    /// The handoff was refused (state drift, missing evidence, ...).
    HandoffRefused,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HandoffStage {
    Prepared,
    Verified,
    Accepted,
    Refused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementRef {
    pub id: String,
    pub summary: String,
    /// `active` | `superseded` | `unknown`
    pub state: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitEvidence {
    pub root: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty: bool,
    pub dirty_files: Vec<String>,
    pub captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub path: String,
    pub absolute_path: String,
    pub kind: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    /// Whether the receiver could actually read the file when verifying.
    pub accessible: Option<bool>,
    /// Whether a verified backup exists. Registering is *not* a backup.
    pub backup_verified: bool,
    pub note: String,
    pub registered_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffVerificationCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub id: String,
    pub task_id: String,
    pub from_agent: String,
    pub to_agent: Option<String>,
    pub stage: HandoffStage,
    /// What the task is and what the goal is.
    pub objective: String,
    pub requirements: Vec<RequirementRef>,
    pub forbidden: Vec<String>,
    pub completed: Vec<String>,
    pub decisions: Vec<String>,
    pub failed_attempts: Vec<String>,
    pub git_evidence: GitEvidence,
    pub artifacts: Vec<ArtifactRef>,
    pub remaining: Vec<String>,
    pub next_steps: Vec<String>,
    pub created_at: String,
    pub verified_at: Option<String>,
    pub accepted_at: Option<String>,
    pub checks: Vec<HandoffVerificationCheck>,
    /// Free-form note written by the receiving agent.
    pub receiver_note: Option<String>,
    /// True when the receiver actually re-read the repository and confirmed state.
    pub native_session_restored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageBody {
    User {
        text: String,
    },
    Agent {
        text: String,
    },
    Thought {
        text: String,
    },
    Tool {
        tool_call_id: String,
        title: String,
        tool_kind: String,
        status: String,
        locations: Vec<String>,
        raw_input: Option<String>,
        raw_output: Option<String>,
    },
    Plan {
        entries: Vec<String>,
    },
    Permission {
        request_id: String,
        title: String,
        options: Vec<PermissionChoice>,
        /// `pending` | option id | `cancelled`
        outcome: String,
    },
    Mode {
        mode: String,
    },
    Usage {
        text: String,
    },
    System {
        text: String,
    },
    Error {
        text: String,
    },
    /// Marks an attempt to restore the agent's own session.
    SessionLink {
        native_session_id: String,
        restored: bool,
        detail: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionChoice {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub body: MessageBody,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub agent_id: String,
    pub title: String,
    pub status: TaskStatus,
    /// The agent's *own* session id. Deliberately namespaced per agent: it is
    /// never a portable session handle.
    pub native_session_id: Option<String>,
    pub native_session_restored: bool,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<Message>,
    pub handoff: Option<Handoff>,
    pub last_git: Option<GitEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub project_id: String,
    pub agent_id: String,
    pub title: String,
    pub status: TaskStatus,
    pub updated_at: String,
    pub message_count: usize,
    pub has_handoff: bool,
    pub live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    pub theme: String,
    pub locale: String,
    pub agent_overrides: std::collections::BTreeMap<String, AgentOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoreState {
    pub projects: Vec<Project>,
    pub tasks: Vec<TaskSummary>,
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Streamed text, thought or user chunk.
    Chunk {
        stream: String,
        text: String,
    },
    ToolCall {
        tool: MessageBody,
    },
    Plan {
        entries: Vec<String>,
    },
    PermissionRequest {
        request_id: String,
        title: String,
        options: Vec<PermissionChoice>,
    },
    PermissionResolved {
        request_id: String,
        outcome: String,
    },
    Mode {
        mode: String,
    },
    Usage {
        text: String,
    },
    NativeSession {
        native_session_id: String,
        restored: bool,
        detail: String,
    },
    Status {
        status: String,
        message: String,
    },
    PromptDone {
        stop_reason: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub task_id: String,
    pub event: SessionEvent,
}
