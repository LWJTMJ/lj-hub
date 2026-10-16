// TypeScript mirror of the Rust model (src-tauri/src/model.rs).
// Kept hand-written and small: no code generation step to go stale.

export type AgentStatus =
  | "AVAILABLE"
  | "NOT_INSTALLED"
  | "AUTH_REQUIRED"
  | "CONFIG_REQUIRED"
  | "UNSUPPORTED"
  | "ERROR"
  | "NOT_TESTED";

export type TransportKind = "acp_stdio" | "open_api_http";

export interface AgentCapabilitiesReport {
  load_session: boolean;
  resume_session: boolean;
  list_sessions: boolean;
  prompt_image: boolean;
  prompt_audio: boolean;
  prompt_embedded_context: boolean;
  terminal: boolean;
}

export interface AgentReport {
  id: string;
  display_name: string;
  vendor: string;
  transport: TransportKind;
  status: AgentStatus;
  executable: string | null;
  version: string | null;
  acp_args: string[];
  overridden: boolean;
  message: string;
  auth_methods: string[];
  agent_info: string | null;
  capabilities: AgentCapabilitiesReport;
  doc_url: string;
  notes: string;
  last_tested_at: string | null;
}

export interface Project {
  id: string;
  name: string;
  path: string;
  git_root: string | null;
  git_remote: string | null;
  created_at: string;
  last_opened_at: string;
}

export interface GitFileChange {
  status: string;
  path: string;
  staged: boolean;
}

export interface GitState {
  is_repo: boolean;
  root: string | null;
  branch: string | null;
  head: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  dirty: boolean;
  changes: GitFileChange[];
  untracked: string[];
  error: string | null;
}

export type TaskStatus =
  | "ACTIVE"
  | "HANDOFF_PREPARED"
  | "HANDOFF_ACCEPTED"
  | "HANDOFF_REFUSED"
  | "CLOSED";

export type HandoffStage = "PREPARED" | "VERIFIED" | "ACCEPTED" | "REFUSED";

export interface PermissionChoice {
  option_id: string;
  name: string;
  kind: string;
}

export type MessageBody =
  | { kind: "user"; text: string }
  | { kind: "agent"; text: string }
  | { kind: "thought"; text: string }
  | {
      kind: "tool";
      tool_call_id: string;
      title: string;
      tool_kind: string;
      status: string;
      locations: string[];
      raw_input: string | null;
      raw_output: string | null;
    }
  | { kind: "plan"; entries: string[] }
  | {
      kind: "permission";
      request_id: string;
      title: string;
      options: PermissionChoice[];
      outcome: string;
    }
  | { kind: "mode"; mode: string }
  | { kind: "usage"; text: string }
  | { kind: "system"; text: string }
  | { kind: "error"; text: string }
  | {
      kind: "session_link";
      native_session_id: string;
      restored: boolean;
      detail: string;
    };

export interface Message {
  id: string;
  role: string;
  body: MessageBody;
  created_at: string;
}

export interface RequirementRef {
  id: string;
  summary: string;
  state: string;
  source: string;
}

export interface ArtifactRef {
  id: string;
  path: string;
  absolute_path: string;
  kind: string;
  size: number | null;
  sha256: string | null;
  accessible: boolean | null;
  backup_verified: boolean;
  note: string;
  registered_at: string;
}

export interface GitEvidence {
  root: string | null;
  branch: string | null;
  head: string | null;
  dirty: boolean;
  dirty_files: string[];
  captured_at: string;
}

export interface HandoffVerificationCheck {
  name: string;
  passed: boolean;
  detail: string;
}

export interface Handoff {
  id: string;
  task_id: string;
  from_agent: string;
  to_agent: string | null;
  stage: HandoffStage;
  objective: string;
  requirements: RequirementRef[];
  forbidden: string[];
  completed: string[];
  decisions: string[];
  failed_attempts: string[];
  git_evidence: GitEvidence;
  artifacts: ArtifactRef[];
  remaining: string[];
  next_steps: string[];
  created_at: string;
  verified_at: string | null;
  accepted_at: string | null;
  checks: HandoffVerificationCheck[];
  receiver_note: string | null;
  native_session_restored: boolean;
}

export interface Task {
  id: string;
  project_id: string;
  agent_id: string;
  title: string;
  status: TaskStatus;
  native_session_id: string | null;
  native_session_restored: boolean;
  created_at: string;
  updated_at: string;
  messages: Message[];
  handoff: Handoff | null;
  last_git: GitEvidence | null;
}

export interface TaskSummary {
  id: string;
  project_id: string;
  agent_id: string;
  title: string;
  status: TaskStatus;
  updated_at: string;
  message_count: number;
  has_handoff: boolean;
  live: boolean;
}

export interface SessionEventEnvelope {
  task_id: string;
  event: SessionEvent;
}

export type SessionEvent =
  | { type: "chunk"; stream: string; text: string }
  | { type: "tool_call"; tool: MessageBody }
  | { type: "plan"; entries: string[] }
  | {
      type: "permission_request";
      request_id: string;
      title: string;
      options: PermissionChoice[];
    }
  | { type: "permission_resolved"; request_id: string; outcome: string }
  | { type: "mode"; mode: string }
  | { type: "usage"; text: string }
  | {
      type: "native_session";
      native_session_id: string;
      restored: boolean;
      detail: string;
    }
  | { type: "status"; status: string; message: string }
  | { type: "prompt_done"; stop_reason: string }
  | { type: "error"; message: string };

export interface AppInfo {
  name: string;
  version: string;
  data_dir: string;
  platform: string;
  arch: string;
  git_available: boolean;
  protocol: string;
  screenshot_open?: string | null;
}

export interface SessionStatus {
  live: boolean;
  native_session_id: string | null;
  native_session_restored: boolean;
  has_history: boolean;
  pending_permissions: string[];
}

export interface SessionStartInfo {
  live: boolean;
  native_session_id: string | null;
  native_session_restored: boolean;
  agent_info: string | null;
  auth_methods: string[];
  detail: string;
  launch: string;
}

export interface PromptReply {
  stop_reason: string;
  error: string | null;
}

export interface ArtifactVerification {
  id: string;
  accessible: boolean;
  size: number | null;
  sha256: string | null;
  matches_recorded: boolean | null;
  detail: string;
}

export interface RequirementFile {
  path: string;
  exists: boolean;
  content: string | null;
  size: number | null;
  sha256: string | null;
  modified: string | null;
  read_only: boolean;
}

export interface WorkBuddyStatus {
  configured: boolean;
  has_token: boolean;
  has_client_credentials: boolean;
  base_url: string;
  status: AgentStatus;
  message: string;
  online: boolean | null;
  credentials_path: string;
}

export interface AcceptResult {
  accepted: boolean;
  message: string;
  handoff: Handoff;
}

export interface Settings {
  theme: string;
  locale: string;
  agent_overrides: Record<string, AgentOverride>;
  last_project_id?: string | null;
  last_task_id?: string | null;
}

export interface AgentOverride {
  executable?: string | null;
  args: string[];
  allow_npm_adapter: boolean;
}

export interface HandoffDraftInput {
  to_agent: string | null;
  objective: string;
  requirements: RequirementRef[];
  forbidden: string[];
  completed: string[];
  decisions: string[];
  failed_attempts: string[];
  remaining: string[];
  next_steps: string[];
  artifact_ids: string[];
  register_paths: string[];
}
