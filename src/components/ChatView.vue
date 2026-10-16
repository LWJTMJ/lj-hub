<script setup lang="ts">
// Centre pane: transcript, agent/model selector, composer, stop/retry.
// Only real capabilities are offered: the agent list comes from the Rust core,
// and an agent that has not passed a handshake is clearly marked.

import { computed, nextTick, ref, watch } from "vue";
import type { AgentReport, Message, Task } from "../types";

interface Labels {
  [key: string]: string;
}

const props = defineProps<{
  task: Task | null;
  agents: AgentReport[];
  live: boolean;
  busy: boolean;
  labels: Labels;
}>();

const emit = defineEmits<{
  (e: "send", text: string): void;
  (e: "cancel"): void;
  (e: "start"): void;
  (e: "stop"): void;
  (e: "respond", payload: { requestId: string; optionId: string | null }): void;
  (e: "change-agent", agentId: string): void;
  (e: "rename", payload: { taskId: string; title: string }): void;
  (e: "open-handoff"): void;
  (e: "toggle-panel"): void;
}>();

const draft = ref("");
const scroller = ref<HTMLElement | null>(null);
const editingTitle = ref(false);
const titleDraft = ref("");

const agentName = computed(() => {
  const id = props.task?.agent_id;
  if (!id) return "";
  return props.agents.find((a) => a.id === id)?.display_name ?? id;
});

const agentReport = computed(() =>
  props.task ? props.agents.find((a) => a.id === props.task?.agent_id) ?? null : null,
);

const canSend = computed(
  () => !!props.task && draft.value.trim().length > 0 && !props.busy,
);

const pendingPermissions = computed(() =>
  (props.task?.messages ?? []).filter(
    (m) => m.body.kind === "permission" && (m.body as { outcome: string }).outcome === "pending",
  ),
);

const transcript = computed<Message[]>(() => props.task?.messages ?? []);

watch(
  () => transcript.value.length,
  async () => {
    await nextTick();
    if (scroller.value) scroller.value.scrollTop = scroller.value.scrollHeight;
  },
);

watch(
  () => props.task?.id,
  () => {
    draft.value = "";
    editingTitle.value = false;
  },
);

function submit() {
  if (!canSend.value) return;
  const text = draft.value.trim();
  draft.value = "";
  emit("send", text);
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
    event.preventDefault();
    submit();
  }
}

function startRename() {
  if (!props.task) return;
  titleDraft.value = props.task.title;
  editingTitle.value = true;
}

function commitRename() {
  editingTitle.value = false;
  if (!props.task) return;
  const next = titleDraft.value.trim();
  if (next && next !== props.task.title) {
    emit("rename", { taskId: props.task.id, title: next });
  }
}

function roleLabel(message: Message): string {
  switch (message.body.kind) {
    case "user":
      return props.labels.userMessage;
    case "agent":
      return props.labels.agentMessage;
    case "thought":
      return props.labels.thinking;
    case "tool":
      return props.labels.toolCall;
    case "permission":
      return props.labels.permission;
    case "plan":
      return props.labels.plan;
    case "system":
    case "session_link":
      return props.labels.system;
    case "error":
      return props.labels.error;
    default:
      return message.role;
  }
}

function toolStatusClass(status: string): string {
  if (status === "completed") return "badge ok";
  if (status === "failed") return "badge danger";
  if (status === "in_progress") return "badge warn";
  return "badge";
}

const statusText = computed(() => {
  const report = agentReport.value;
  if (!report) return "";
  return report.status;
});

const statusClass = computed(() => {
  switch (statusText.value) {
    case "AVAILABLE":
      return "badge ok";
    case "AUTH_REQUIRED":
    case "CONFIG_REQUIRED":
      return "badge warn";
    case "ERROR":
    case "NOT_INSTALLED":
      return "badge danger";
    default:
      return "badge";
  }
});
</script>

<template>
  <main class="chat">
    <div v-if="!task" class="empty">
      <p>{{ labels.noProjects }}</p>
    </div>

    <template v-else>
      <header class="chat-header">
        <div class="chat-title">
          <template v-if="editingTitle">
            <input
              v-model="titleDraft"
              style="max-width: 320px"
              @keydown.enter="commitRename"
              @keydown.esc="editingTitle = false"
              @blur="commitRename"
            />
          </template>
          <template v-else>
            <span class="name" @dblclick="startRename">{{ task.title }}</span>
            <button class="ghost" style="padding: 1px 6px" @click="startRename">✎</button>
          </template>
        </div>

        <span :class="statusClass" :title="agentReport?.message">
          {{ agentName }} · {{ statusText }}
        </span>
        <span class="badge" :class="live ? 'ok' : ''">
          {{ live ? labels.sessionLive : labels.sessionOffline }}
        </span>

        <select
          style="width: auto"
          :value="task.agent_id"
          :title="labels.agent"
          @change="emit('change-agent', ($event.target as HTMLSelectElement).value)"
        >
          <option v-for="agent in agents" :key="agent.id" :value="agent.id">
            {{ agent.display_name }}
          </option>
        </select>

        <button v-if="!live" class="primary" :disabled="busy" @click="emit('start')">
          {{ labels.connecting }}
        </button>
        <button v-else class="ghost" @click="emit('stop')">{{ labels.stop }}</button>
        <button class="ghost" @click="emit('open-handoff')">{{ labels.handoff }}</button>
        <button class="ghost" @click="emit('toggle-panel')">⇥</button>
      </header>

      <div ref="scroller" class="messages">
        <div v-if="transcript.length === 0" class="empty">
          <p>{{ labels.messagePlaceholder }}</p>
          <p v-if="agentReport && agentReport.status !== 'AVAILABLE'" class="hint warn">
            {{ agentReport.message }}
          </p>
        </div>

        <div
          v-for="message in transcript"
          :key="message.id"
          class="message"
          :class="message.body.kind === 'session_link' ? 'system' : message.body.kind"
        >
          <div class="avatar" :class="message.body.kind === 'agent' ? 'agent' : ''">
            {{ roleLabel(message).slice(0, 1) }}
          </div>

          <!-- tool call -->
          <div v-if="message.body.kind === 'tool'" class="tool-card">
            <div class="tool-head">
              <span>{{ message.body.title }}</span>
              <span class="tool-kind">{{ message.body.tool_kind }}</span>
              <span :class="toolStatusClass(message.body.status)" style="margin-left: auto">
                {{ message.body.status }}
              </span>
            </div>
            <div v-if="message.body.locations.length" class="tool-body">
              {{ message.body.locations.join("\n") }}
            </div>
            <div v-if="message.body.raw_input" class="tool-body">
              输入：{{ message.body.raw_input }}
            </div>
            <div v-if="message.body.raw_output" class="tool-body">
              输出：{{ message.body.raw_output }}
            </div>
          </div>

          <!-- permission -->
          <div v-else-if="message.body.kind === 'permission'" class="permission-card">
            <div class="role-label">{{ labels.permission }}</div>
            <strong>{{ message.body.title }}</strong>
            <div v-if="message.body.outcome === 'pending'" class="actions">
              <span class="badge warn">{{ labels.permissionPending }}</span>
              <button
                v-for="option in message.body.options"
                :key="option.option_id"
                :class="option.kind.startsWith('allow') ? 'primary' : ''"
                @click="
                  emit('respond', {
                    requestId: message.body.kind === 'permission' ? message.body.request_id : '',
                    optionId: option.option_id,
                  })
                "
              >
                {{ option.name }}
              </button>
              <button
                class="danger"
                @click="
                  emit('respond', {
                    requestId: message.body.kind === 'permission' ? message.body.request_id : '',
                    optionId: null,
                  })
                "
              >
                {{ labels.deny }}
              </button>
            </div>
            <div v-else class="hint">
              结果：{{ message.body.outcome }}
            </div>
          </div>

          <!-- plan -->
          <div v-else-if="message.body.kind === 'plan'" class="bubble">
            <div class="role-label">{{ labels.plan }}</div>
            <div v-for="(entry, index) in message.body.entries" :key="index">
              {{ entry }}
            </div>
          </div>

          <!-- plain text-ish -->
          <div v-else class="bubble">
            <div class="role-label">{{ roleLabel(message) }}</div>
            <template v-if="message.body.kind === 'session_link'">
              {{ message.body.detail }}
              <div class="hint" style="margin-top: 4px">
                {{
                  message.body.restored
                    ? labels.nativeRestored
                    : labels.historyUnavailable
                }}
                · {{ message.body.native_session_id }}
              </div>
            </template>
            <template v-else-if="'text' in message.body">{{ message.body.text }}</template>
            <template v-else-if="'mode' in message.body">模式：{{ message.body.mode }}</template>
          </div>
        </div>
      </div>

      <div class="composer">
        <div class="composer-inner">
          <div v-if="pendingPermissions.length" class="hint warn" style="margin-bottom: 6px">
            {{ labels.permissionPending }} ·
            {{ pendingPermissions.length }}
          </div>
          <textarea
            v-model="draft"
            :placeholder="labels.messagePlaceholder"
            :disabled="busy"
            @keydown="onKeydown"
          />
          <div class="composer-row">
            <span class="composer-hint">⌘/Ctrl + Enter {{ labels.send }}</span>
            <button v-if="busy" class="danger" @click="emit('cancel')">
              {{ labels.cancel }}
            </button>
            <button class="primary" :disabled="!canSend" @click="submit">
              {{ labels.send }}
            </button>
          </div>
        </div>
      </div>
    </template>
  </main>
</template>
