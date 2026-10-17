<script setup lang="ts">
// LJ Hub main window: left (projects/tasks), centre (chat), right (project,
// Git, handoff). All data comes from the Rust core; nothing is faked.

import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { api, errorText, onSessionEvent, pickDirectory } from "./api";
import { t, type Locale } from "./i18n";
import type {
  AgentReport,
  AppInfo,
  GitState,
  Message,
  Project,
  SessionEventEnvelope,
  Settings,
  Task,
  TaskSummary,
} from "./types";
import Sidebar from "./components/Sidebar.vue";
import ChatView from "./components/ChatView.vue";
import RightPanel from "./components/RightPanel.vue";
import RunsDialog from "./components/RunsDialog.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import HandoffDialog from "./components/HandoffDialog.vue";
import Toasts from "./components/Toasts.vue";

const info = ref<AppInfo | null>(null);
const settings = ref<Settings>({ theme: "system", locale: "zh-CN", agent_overrides: {} });
const projects = ref<Project[]>([]);
const tasks = ref<TaskSummary[]>([]);
const agents = ref<AgentReport[]>([]);
const currentProjectId = ref<string | null>(null);
const currentTaskId = ref<string | null>(null);
const task = ref<Task | null>(null);
const git = ref<GitState | null>(null);
const live = ref(false);
const busy = ref(false);
const toast = ref<string | null>(null);
const showSettings = ref(false);
const showHandoff = ref(false);
const showRuns = ref(false);
const runsBadge = ref(0);
const paneCollapsed = ref(false);
const toasts = ref<{ id: number; text: string; kind: "info" | "error" }[]>([]);

let unlisten: (() => void) | null = null;

const locale = computed<Locale>(() => (settings.value.locale === "en" ? "en" : "zh-CN"));
const T = computed(() => t(locale.value));
const currentProject = computed(
  () => projects.value.find((p) => p.id === currentProjectId.value) ?? null,
);

function notify(text: string, kind: "info" | "error" = "info") {
  const id = Date.now() + Math.random();
  toasts.value.push({ id, text, kind });
  window.setTimeout(() => {
    toasts.value = toasts.value.filter((item) => item.id !== id);
  }, 4200);
}

function fail(error: unknown, prefix?: string) {
  const text = errorText(error);
  toasts.value = toasts.value.filter(() => true);
  notify(prefix ? `${prefix}: ${text}` : text, "error");
  toast.value = null;
}

function applyTheme() {
  const mode = settings.value.theme;
  const prefersDark =
    window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
  const dark = mode === "dark" || (mode === "system" && prefersDark);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

async function loadSettings() {
  try {
    settings.value = await api.getSettings();
  } catch (error) {
    fail(error);
  }
  applyTheme();
}

async function loadProjects() {
  projects.value = await api.listProjects();
  if (!currentProjectId.value && projects.value.length > 0) {
    currentProjectId.value = projects.value[0].id;
  }
}

async function loadAgents() {
  try {
    agents.value = await api.listAgents();
  } catch (error) {
    fail(error);
  }
}

async function loadTasks() {
  if (!currentProjectId.value) {
    tasks.value = [];
    return;
  }
  tasks.value = await api.listTasks(currentProjectId.value);
}

async function loadGit() {
  if (!currentProjectId.value) {
    git.value = null;
    return;
  }
  try {
    git.value = await api.gitState(currentProjectId.value);
  } catch (error) {
    fail(error);
  }
}

async function openTask(taskId: string, remember = true) {
  currentTaskId.value = taskId;
  if (remember) void rememberLocation();
  try {
    task.value = await api.getTask(taskId);
    const status = await api.sessionStatus(taskId);
    live.value = status.live;
  } catch (error) {
    fail(error);
  }
}

async function refreshTaskSoon() {
  if (!currentTaskId.value) return;
  try {
    task.value = await api.getTask(currentTaskId.value);
  } catch (error) {
    fail(error);
  }
}

async function addProject() {
  const path = await pickDirectory();
  if (!path) return;
  try {
    const project = await api.addProject(path);
    await loadProjects();
    currentProjectId.value = project.id;
    notify(`${project.name} 已加入`);
  } catch (error) {
    fail(error, "打开项目失败");
  }
}

async function removeProject(projectId: string) {
  try {
    await api.removeProject(projectId);
    if (currentProjectId.value === projectId) {
      currentProjectId.value = null;
      currentTaskId.value = null;
      task.value = null;
    }
    await loadProjects();
    notify(T.value.removeProjectHint);
  } catch (error) {
    fail(error);
  }
}

async function createTask(agentId: string) {
  if (!currentProjectId.value) {
    notify(T.value.noProjects, "error");
    return;
  }
  try {
    const created = await api.createTask(
      currentProjectId.value,
      agentId,
      `新任务 · ${agentDisplay(agentId)}`,
    );
    await loadTasks();
    await openTask(created.id);
  } catch (error) {
    fail(error, "创建任务失败");
  }
}

async function deleteTask(taskId: string) {
  if (!window.confirm(T.value.deleteTaskConfirm)) return;
  try {
    await api.deleteTask(taskId);
    if (currentTaskId.value === taskId) {
      currentTaskId.value = null;
      task.value = null;
      live.value = false;
    }
    await loadTasks();
  } catch (error) {
    fail(error);
  }
}

async function renameTask(payload: { taskId: string; title: string }) {
  try {
    const updated = await api.renameTask(payload.taskId, payload.title);
    if (task.value && task.value.id === updated.id) task.value = updated;
    await loadTasks();
  } catch (error) {
    fail(error);
  }
}

async function send(text: string) {
  if (!currentTaskId.value) return;
  busy.value = true;
  try {
    // The user's own message is recorded by the core before the agent runs.
    await api.sendPrompt(currentTaskId.value, text);
  } catch (error) {
    fail(error, "发送失败");
  } finally {
    busy.value = false;
    live.value = (await api.sessionStatus(currentTaskId.value)).live;
    await refreshTaskSoon();
    await loadTasks();
  }
}

async function cancel() {
  if (!currentTaskId.value) return;
  try {
    await api.cancelPrompt(currentTaskId.value);
  } catch (error) {
    fail(error, "取消失败");
  }
}

async function respond(permission: { requestId: string; optionId: string | null }) {
  if (!currentTaskId.value) return;
  try {
    const resolved = await api.respondPermission(
      currentTaskId.value,
      permission.requestId,
      permission.optionId,
    );
    if (!resolved) notify("该权限请求已处理", "error");
    await refreshTaskSoon();
  } catch (error) {
    fail(error);
  }
}

async function startSession() {
  if (!currentTaskId.value) return;
  busy.value = true;
  try {
    const result = await api.startSession(currentTaskId.value);
    live.value = result.live;
    notify(result.live ? T.value.sessionStarted : T.value.sessionOffline);
    await refreshTaskSoon();
  } catch (error) {
    fail(error, "建立会话失败");
  } finally {
    busy.value = false;
  }
}

async function stopSession() {
  if (!currentTaskId.value) return;
  try {
    await api.stopSession(currentTaskId.value);
    live.value = false;
    await refreshTaskSoon();
  } catch (error) {
    fail(error);
  }
}

async function changeAgent(agentId: string) {
  // A new agent means a new session: LJ Hub never pretends that one agent's
  // native session belongs to another.
  await createTask(agentId);
}

function agentDisplay(agentId: string): string {
  return agents.value.find((a) => a.id === agentId)?.display_name ?? agentId;
}

async function setTheme(theme: string) {
  settings.value.theme = theme;
  applyTheme();
  try {
    await api.setTheme(theme);
  } catch (error) {
    fail(error);
  }
}

async function setLocale(next: string) {
  settings.value.locale = next;
  try {
    await api.setLocale(next);
  } catch (error) {
    fail(error);
  }
}

function handleEvent(envelope: SessionEventEnvelope) {
  if (!currentTaskId.value || envelope.task_id !== currentTaskId.value) return;
  const event = envelope.event;
  // Incremental UI update; the durable copy is already written by the core.
  const current = task.value;
  if (!current) return;
  if (event.type === "chunk") {
    const stream = event.stream;
    const last = [...current.messages].reverse().find((m) => m.role === stream);
    const body = { kind: stream as "agent", text: event.text } as Message["body"];
    if (last && (last.body.kind === "agent" || last.body.kind === "thought" || last.body.kind === "user")) {
      const lastIndex = current.messages.indexOf(last);
      const merged = { ...last, body: { ...last.body, text: (last.body as { text: string }).text + event.text } } as Message;
      current.messages.splice(lastIndex, 1, merged);
    } else {
      current.messages.push({
        id: `live-${Date.now()}-${Math.random()}`,
        role: stream,
        body,
        created_at: new Date().toISOString(),
      });
    }
  } else if (event.type === "tool_call") {
    void refreshTaskSoon();
  } else if (event.type === "permission_request") {
    void refreshTaskSoon();
  } else if (event.type === "prompt_done" || event.type === "error" || event.type === "native_session") {
    void refreshTaskSoon();
    void loadTasks();
    if (event.type === "error") notify(event.message, "error");
  } else if (event.type === "status") {
    // Informational only; the transcript keeps the durable copy.
  }
}

/// Surface runs that stopped for a human, without opening anything.
async function refreshRunsBadge() {
  try {
    const blocked = await api.runsNeedingHuman();
    runsBadge.value = blocked.length;
    if (blocked.length > 0) {
      notify(`有 ${blocked.length} 个接力运行需要你确认`, "error");
    }
  } catch {
    // A missing badge must never break the window.
  }
}

async function rememberLocation() {
  try {
    await api.setLastOpened(currentProjectId.value, currentTaskId.value);
  } catch {
    // Remembering the last location is a convenience; never surface failures.
  }
}

watch(currentProjectId, async () => {
  await loadGit();
  await loadTasks();
});

async function restoreLocation() {
  const projectId = settings.value.last_project_id ?? null;
  if (projectId && projects.value.some((p) => p.id === projectId)) {
    currentProjectId.value = projectId;
    await loadTasks();
    const taskId = settings.value.last_task_id ?? null;
    if (taskId && tasks.value.some((t) => t.id === taskId)) {
      await openTask(taskId, false);
    }
  }
}

onMounted(async () => {
  try {
    info.value = await api.appInfo();
  } catch (error) {
    fail(error);
  }
  await loadSettings();
  await loadProjects();
  await loadAgents();
  await loadGit();
  await loadTasks();
  await restoreLocation();
  await refreshRunsBadge();
  // Documentation helper: LJ_HUB_SCREENSHOT_OPEN=settings opens that view
  // immediately so docs screenshots need no synthetic clicks.
  if (info.value?.screenshot_open === "settings") showSettings.value = true;
  if (info.value?.screenshot_open === "handoff") showHandoff.value = true;
  unlisten = await onSessionEvent(handleEvent);
  window
    .matchMedia?.("(prefers-color-scheme: dark)")
    .addEventListener?.("change", applyTheme);
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <div class="app">
    <Sidebar
      :info="info"
      :projects="projects"
      :tasks="tasks"
      :agents="agents"
      :current-project-id="currentProjectId"
      :current-task-id="currentTaskId"
      :live="live"
      :labels="T"
      @add-project="addProject"
      @remove-project="removeProject"
      @select-project="(id: string) => (currentProjectId = id)"
      @select-task="openTask"
      @create-task="createTask"
      @delete-task="deleteTask"
      @open-settings="showSettings = true"
      @open-runs="
        async () => {
          showRuns = true;
        }
      "
      :runs-badge="runsBadge"
    />

    <ChatView
      :task="task"
      :agents="agents"
      :live="live"
      :busy="busy"
      :labels="T"
      @send="send"
      @cancel="cancel"
      @start="startSession"
      @stop="stopSession"
      @respond="respond"
      @change-agent="changeAgent"
      @rename="renameTask"
      @open-handoff="showHandoff = true"
      @toggle-panel="paneCollapsed = !paneCollapsed"
    />

    <RightPanel
      v-model:collapsed="paneCollapsed"
      :task="task"
      :project="currentProject"
      :git="git"
      :labels="T"
      @refresh-git="loadGit"
      @open-handoff="showHandoff = true"
    />
  </div>

  <SettingsDialog
    v-if="showSettings"
    :agents="agents"
    :settings="settings"
    :info="info"
    :labels="T"
    @close="showSettings = false"
    @refresh="loadAgents"
    @theme="setTheme"
    @locale="setLocale"
  />

  <RunsDialog
    v-if="showRuns"
    :project-id="currentProjectId"
    :agents="agents"
    :labels="T"
    @close="
      async () => {
        showRuns = false;
        await loadTasks();
      }
    "
    @open-task="openTask"
  />

  <HandoffDialog
    v-if="showHandoff && task"
    :task="task"
    :agents="agents"
    :labels="T"
    @close="showHandoff = false"
    @changed="
      async () => {
        await refreshTaskSoon();
        await loadTasks();
      }
    "
  />

  <Toasts :items="toasts" />
</template>
