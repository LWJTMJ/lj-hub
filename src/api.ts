// Thin typed wrapper over the Tauri command surface. Every function here maps
// to a real Rust command; there are no mock fallbacks in production paths.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AcceptResult,
  CreateRunInput,
  RelayRun,
  RunDetail,
  RunSummary,
  AgentReport,
  AppInfo,
  ArtifactRef,
  ArtifactVerification,
  GitState,
  Handoff,
  HandoffDraftInput,
  HandoffVerificationCheck,
  Project,
  PromptReply,
  RequirementFile,
  RequirementRef,
  SessionEventEnvelope,
  SessionStartInfo,
  SessionStatus,
  Settings,
  Task,
  TaskSummary,
  WorkBuddyStatus,
} from "./types";

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),

  // projects
  listProjects: () => invoke<Project[]>("list_projects"),
  addProject: (path: string) => invoke<Project>("add_project", { path }),
  removeProject: (projectId: string) =>
    invoke<void>("remove_project", { projectId }),
  gitState: (projectId: string) => invoke<GitState>("git_state", { projectId }),
  gitDiff: (projectId: string, path: string) =>
    invoke<string>("git_diff", { projectId, path }),
  recentCommits: (projectId: string, count = 10) =>
    invoke<string[]>("recent_commits", { projectId, count }),

  // agents
  listAgents: () => invoke<AgentReport[]>("list_agents"),
  detectAgents: () => invoke<AgentReport[]>("detect_agents"),
  testAgent: (agentId: string) => invoke<AgentReport>("test_agent", { agentId }),
  setAgentOverride: (
    agentId: string,
    executable: string | null,
    args: string[],
    allowNpmAdapter: boolean,
    envPassthrough: string[] = [],
  ) =>
    invoke<void>("set_agent_override", {
      agentId,
      executable,
      args,
      allowNpmAdapter,
      envPassthrough,
    }),
  verifyAgentTurn: (agentId: string) =>
    invoke<AgentReport>("verify_agent_turn", { agentId }),
  clearAgentOverride: (agentId: string) =>
    invoke<void>("clear_agent_override", { agentId }),

  // workbuddy
  workbuddyStatusDetail: () =>
    invoke<WorkBuddyStatus>("workbuddy_status_detail"),
  workbuddyAuthorizeUrl: (scope?: string) =>
    invoke<string>("workbuddy_authorize_url", { scope: scope ?? null }),
  workbuddyExchangeCode: (code: string, redirectUri: string) =>
    invoke<void>("workbuddy_exchange_code", { code, redirectUri }),
  workbuddySaveToken: (payload: {
    accessToken: string;
    clientId?: string | null;
    clientSecret?: string | null;
    redirectUri?: string | null;
  }) => invoke<void>("workbuddy_save_token", payload),

  // tasks
  listTasks: (projectId: string) =>
    invoke<TaskSummary[]>("list_tasks", { projectId }),
  createTask: (projectId: string, agentId: string, title: string) =>
    invoke<Task>("create_task", { projectId, agentId, title }),
  getTask: (taskId: string) => invoke<Task>("get_task", { taskId }),
  renameTask: (taskId: string, title: string) =>
    invoke<Task>("rename_task", { taskId, title }),
  deleteTask: (taskId: string) => invoke<void>("delete_task", { taskId }),

  // sessions
  startSession: (taskId: string) =>
    invoke<SessionStartInfo>("start_session", { taskId }),
  sendPrompt: (taskId: string, text: string) =>
    invoke<PromptReply>("send_prompt", { taskId, text }),
  cancelPrompt: (taskId: string) => invoke<void>("cancel_prompt", { taskId }),
  respondPermission: (
    taskId: string,
    requestId: string,
    optionId: string | null,
  ) =>
    invoke<boolean>("respond_permission", { taskId, requestId, optionId }),
  sessionStatus: (taskId: string) =>
    invoke<SessionStatus>("session_status", { taskId }),
  stopSession: (taskId: string) => invoke<void>("stop_session", { taskId }),

  // handoff
  prepareHandoff: (taskId: string, draft: HandoffDraftInput) =>
    invoke<Handoff>("prepare_handoff", { taskId, draft }),
  verifyHandoff: (taskId: string) =>
    invoke<HandoffVerificationCheck[]>("verify_handoff", { taskId }),
  acceptHandoff: (taskId: string, receiverAgent: string, note: string) =>
    invoke<AcceptResult>("accept_handoff", { taskId, receiverAgent, note }),
  handoffMarkdown: (taskId: string) =>
    invoke<string>("handoff_markdown", { taskId }),
  registerArtifact: (
    taskId: string,
    path: string,
    kind: string,
    note: string,
    allowLarge: boolean,
  ) =>
    invoke<ArtifactRef>("register_artifact", {
      taskId,
      path,
      kind,
      note,
      allowLarge,
    }),
  verifyArtifact: (taskId: string, artifactId: string, forceHash: boolean) =>
    invoke<ArtifactVerification>("verify_artifact", {
      taskId,
      artifactId,
      forceHash,
    }),
  removeArtifact: (taskId: string, artifactId: string) =>
    invoke<void>("remove_artifact", { taskId, artifactId }),

  // existing workspaces documents (read-only)
  readRequirementFile: (taskId: string, relativePath: string) =>
    invoke<RequirementFile>("read_requirement_file", { taskId, relativePath }),
  importRequirements: (taskId: string, relativePath: string) =>
    invoke<RequirementRef[]>("import_requirements", { taskId, relativePath }),

  // relay runs
  listRuns: () => invoke<RunSummary[]>("list_runs"),
  getRun: (runId: string) => invoke<RunDetail>("get_run", { runId }),
  createRun: (input: CreateRunInput) =>
    invoke<RelayRun>("create_run", { input }),
  planRun: (runId: string) => invoke<RelayRun>("plan_run", { runId }),
  startRun: (runId: string) => invoke<RelayRun>("start_run", { runId }),
  pauseRun: (runId: string) => invoke<RelayRun>("pause_run", { runId }),
  cancelRun: (runId: string) => invoke<RelayRun>("cancel_run", { runId }),
  resolveRunStep: (
    runId: string,
    stepId: string,
    action: "retry" | "mark_done" | "cancel",
    note: string,
  ) => invoke<RelayRun>("resolve_run_step", { runId, stepId, action, note }),
  deleteRun: (runId: string) => invoke<void>("delete_run", { runId }),
  runsNeedingHuman: () =>
    invoke<[string, string, string][]>("runs_needing_human"),

  // settings
  getSettings: () => invoke<Settings>("get_settings"),
  setLastOpened: (projectId: string | null, taskId: string | null) =>
    invoke<void>("set_last_opened", { projectId, taskId }),
  setTheme: (theme: string) => invoke<void>("set_theme", { theme }),
  setLocale: (locale: string) => invoke<void>("set_locale", { locale }),
};

export async function pickDirectory(): Promise<string | null> {
  const result = await open({ directory: true, multiple: false });
  if (typeof result === "string") return result;
  return null;
}

export function onRunEvent(
  handler: (payload: { run_id: string; message: string }) => void,
): Promise<UnlistenFn> {
  return listen<{ run_id: string; message: string }>("lj://run-event", (event) =>
    handler(event.payload),
  );
}

export function onSessionEvent(
  handler: (payload: SessionEventEnvelope) => void,
): Promise<UnlistenFn> {
  return listen<SessionEventEnvelope>("lj://session-event", (event) =>
    handler(event.payload),
  );
}

/// Human readable error text for anything thrown by `invoke`.
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}
