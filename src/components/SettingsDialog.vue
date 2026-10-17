<script setup lang="ts">
// Agent management: real detection results, executable override, real ACP
// connection test, and the WorkBuddy Open API credential flow. Anything not
// verified is labelled NOT_TESTED; nothing is claimed without evidence.

import { onMounted, ref } from "vue";
import { api, errorText } from "../api";
import type { AgentReport, AppInfo, Settings, WorkBuddyStatus } from "../types";

interface Labels {
  [key: string]: string;
}

const props = defineProps<{
  agents: AgentReport[];
  settings: Settings;
  info: AppInfo | null;
  labels: Labels;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "refresh"): void;
  (e: "theme", theme: string): void;
  (e: "locale", locale: string): void;
}>();

const editing = ref<string | null>(null);
const execDraft = ref("");
const argsDraft = ref("");
const allowNpm = ref(false);
const envDraft = ref("");
const testing = ref<string | null>(null);
const verifying = ref<string | null>(null);
const reports = ref<AgentReport[]>(props.agents);
const workbuddy = ref<WorkBuddyStatus | null>(null);
const tokenDraft = ref("");
const clientIdDraft = ref("");
const clientSecretDraft = ref("");
const redirectDraft = ref("");
const codeDraft = ref("");
const authUrl = ref("");
const message = ref("");
const error = ref("");

onMounted(async () => {
  reports.value = await api.detectAgents();
  try {
    workbuddy.value = await api.workbuddyStatusDetail();
  } catch (e) {
    error.value = errorText(e);
  }
});

function statusClass(status: string): string {
  switch (status) {
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
}

function beginEdit(agent: AgentReport) {
  editing.value = agent.id;
  execDraft.value = agent.executable ?? "";
  argsDraft.value = agent.acp_args.join(" ");
  allowNpm.value = props.settings.agent_overrides?.[agent.id]?.allow_npm_adapter ?? false;
  envDraft.value = (props.settings.agent_overrides?.[agent.id]?.env_passthrough ?? []).join(" ");
  message.value = "";
  error.value = "";
}

async function saveOverride(agentId: string) {
  try {
    await api.setAgentOverride(
      agentId,
      execDraft.value.trim() === "" ? null : execDraft.value.trim(),
      argsDraft.value.split(/\s+/).filter((s) => s.length > 0),
      allowNpm.value,
      envDraft.value.split(/\s+/).filter((s) => s.length > 0),
    );
    editing.value = null;
    await refresh();
  } catch (e) {
    error.value = errorText(e);
  }
}

async function clearOverride(agentId: string) {
  try {
    await api.clearAgentOverride(agentId);
    editing.value = null;
    await refresh();
  } catch (e) {
    error.value = errorText(e);
  }
}

async function refresh() {
  reports.value = await api.detectAgents();
  emit("refresh");
}

/// Prove a real model turn. Spends the user's quota, so it is never automatic.
async function runTurnVerification(agentId: string) {
  verifying.value = agentId;
  message.value = "";
  error.value = "";
  try {
    const report = await api.verifyAgentTurn(agentId);
    const index = reports.value.findIndex((a) => a.id === agentId);
    if (index >= 0) reports.value.splice(index, 1, report);
    message.value = `${report.display_name}: ${report.message}`;
    emit("refresh");
  } catch (e) {
    error.value = errorText(e);
  } finally {
    verifying.value = null;
  }
}

async function runTest(agentId: string) {
  testing.value = agentId;
  message.value = "";
  error.value = "";
  try {
    const report = await api.testAgent(agentId);
    const index = reports.value.findIndex((a) => a.id === agentId);
    if (index >= 0) reports.value.splice(index, 1, report);
    message.value = `${report.display_name}: ${report.status} — ${report.message}`;
    emit("refresh");
  } catch (e) {
    error.value = errorText(e);
  } finally {
    testing.value = null;
  }
}

async function saveToken() {
  error.value = "";
  message.value = "";
  try {
    await api.workbuddySaveToken({
      accessToken: tokenDraft.value,
      clientId: clientIdDraft.value || null,
      clientSecret: clientSecretDraft.value || null,
      redirectUri: redirectDraft.value || null,
    });
    tokenDraft.value = "";
    clientSecretDraft.value = "";
    workbuddy.value = await api.workbuddyStatusDetail();
    message.value = `凭据已保存在本机：${workbuddy.value.credentials_path}`;
  } catch (e) {
    error.value = errorText(e);
  }
}

async function buildAuthUrl() {
  error.value = "";
  try {
    authUrl.value = await api.workbuddyAuthorizeUrl("user.localassistant.readable user.localassistant.invokable user.task.readable user.task.invokable");
    message.value = "请在浏览器中打开上面的链接完成授权，然后把回调地址中的 code 填入下方。";
  } catch (e) {
    error.value = errorText(e);
  }
}

async function exchange() {
  error.value = "";
  try {
    await api.workbuddyExchangeCode(codeDraft.value, redirectDraft.value);
    workbuddy.value = await api.workbuddyStatusDetail();
    message.value = "已用授权码换取令牌。";
    codeDraft.value = "";
  } catch (e) {
    error.value = errorText(e);
  }
}

async function refreshWorkbuddy() {
  try {
    workbuddy.value = await api.workbuddyStatusDetail();
  } catch (e) {
    error.value = errorText(e);
  }
}
</script>

<template>
  <div class="modal-backdrop" @click.self="emit('close')">
    <div class="modal wide">
      <div class="modal-head">
        <span class="modal-title">{{ labels.settings }}</span>
        <button class="ghost" @click="emit('close')">{{ labels.close }}</button>
      </div>

      <div class="modal-body">
        <div v-if="message" class="card" style="border-color: var(--ok); margin-bottom: 10px">
          {{ message }}
        </div>
        <div v-if="error" class="card" style="border-color: var(--danger); color: var(--danger); margin-bottom: 10px">
          {{ error }}
        </div>

        <h4 style="margin: 0 0 8px">{{ labels.agents }}</h4>
        <p class="hint" style="margin-bottom: 10px">
          {{ labels.notTestedNotice }}
        </p>

        <div v-for="agent in reports" :key="agent.id" class="agent-row">
          <div>
            <div class="agent-name">
              <span>{{ agent.display_name }}</span>
              <span :class="statusClass(agent.status)">{{ agent.status }}</span>
              <span v-if="agent.overridden" class="badge">自定义路径</span>
            </div>
            <div class="agent-notes">{{ agent.message }}</div>
            <div class="agent-meta">
              <div>vendor: {{ agent.vendor }}</div>
              <div v-if="agent.executable">exe: {{ agent.executable }}</div>
              <div v-if="agent.version">version: {{ agent.version }}</div>
              <div v-if="agent.acp_args.length">acp: {{ agent.executable }} {{ agent.acp_args.join(" ") }}</div>
              <div v-if="agent.agent_info">handshake: {{ agent.agent_info }}</div>
              <div v-if="agent.auth_methods.length">
                auth: {{ agent.auth_methods.join(", ") }}
              </div>
              <div v-if="agent.last_tested_at">tested: {{ agent.last_tested_at }}</div>
              <div>
                验证：可执行 {{ agent.verification?.executable_resolved ? "✓" : "—" }} ·
                握手 {{ agent.verification?.handshake_ok ? "✓" : "—" }} ·
                真实回合
                {{
                  agent.verification?.real_turn_ok === true
                    ? "✓"
                    : agent.verification?.real_turn_ok === false
                      ? "✗"
                      : "未验证"
                }} ·
                <span :class="agent.verification?.schedulable ? 'badge ok' : 'badge warn'">
                  {{ agent.verification?.schedulable ? "可用于自动接力" : "不可用于自动接力" }}
                </span>
              </div>
              <div v-if="agent.verification?.real_turn_error" style="color: var(--danger)">
                真实回合错误：{{ agent.verification.real_turn_error }}
              </div>
              <div>
                能力：load_session={{ agent.capabilities.load_session }} ·
                resume={{ agent.capabilities.resume_session }} ·
                list={{ agent.capabilities.list_sessions }} ·
                image={{ agent.capabilities.prompt_image }} ·
                terminal={{ agent.capabilities.terminal }}
              </div>
            </div>
            <div class="agent-notes">{{ agent.notes }}</div>

            <div v-if="editing === agent.id" class="card" style="margin-top: 8px">
              <label class="field">
                <span>{{ labels.executable }}</span>
                <input v-model="execDraft" placeholder="/path/to/agent" />
              </label>
              <label class="field">
                <span>启动参数（空格分隔）</span>
                <input v-model="argsDraft" placeholder="acp" />
              </label>
              <label class="field">
                <span>
                  透传环境变量名（空格分隔；只保存变量名，值仅在启动时从环境读取，不落盘不回显）
                </span>
                <input v-model="envDraft" placeholder="DEEPSEEK_API_KEY" />
              </label>
              <label class="checkline">
                <input v-model="allowNpm" type="checkbox" />
                <span>
                  允许通过固定版本 npx 适配器启动（不会使用 latest）
                </span>
              </label>
              <div style="display: flex; gap: 8px; margin-top: 8px">
                <button class="primary" @click="saveOverride(agent.id)">{{ labels.save }}</button>
                <button @click="clearOverride(agent.id)">{{ labels.clear }}</button>
                <button class="ghost" @click="editing = null">{{ labels.cancel }}</button>
              </div>
            </div>
          </div>

          <div style="display: flex; flex-direction: column; gap: 6px">
            <button
              class="primary"
              :disabled="testing === agent.id"
              @click="runTest(agent.id)"
            >
              <span v-if="testing === agent.id" class="spinner" />
              {{ labels.testConnection }}
            </button>
            <button
              :disabled="verifying === agent.id || agent.status === 'NOT_INSTALLED'"
              :title="'会真实调用一次模型（消耗额度），通过后才允许参与自动接力'"
              @click="runTurnVerification(agent.id)"
            >
              <span v-if="verifying === agent.id" class="spinner" />
              验证真实回合
            </button>
            <button @click="beginEdit(agent)">配置</button>
            <a :href="agent.doc_url" target="_blank" style="font-size: 11.5px; color: var(--text-dim)">
              官方文档
            </a>
          </div>
        </div>

        <h4 style="margin: 18px 0 8px">{{ labels.workbuddyTitle }}</h4>
        <div class="card">
          <p class="hint">{{ labels.workbuddyNeedsApproval }}</p>
          <div v-if="workbuddy" class="agent-meta" style="margin: 6px 0">
            <div>status: {{ workbuddy.status }}</div>
            <div>base_url: {{ workbuddy.base_url }}</div>
            <div>token: {{ workbuddy.has_token ? "已配置" : "未配置" }}</div>
            <div>client 凭据: {{ workbuddy.has_client_credentials ? "已配置" : "未配置" }}</div>
            <div>凭据文件: {{ workbuddy.credentials_path }}</div>
            <div v-if="workbuddy.online !== null">本地助理在线: {{ workbuddy.online }}</div>
          </div>
          <p v-if="workbuddy" class="hint warn">{{ workbuddy.message }}</p>

          <div class="grid-2">
            <label class="field">
              <span>{{ labels.workbuddyClientId }}</span>
              <input v-model="clientIdDraft" placeholder="app_xxx" />
            </label>
            <label class="field">
              <span>{{ labels.workbuddyRedirect }}</span>
              <input v-model="redirectDraft" placeholder="https://example.com/callback" />
            </label>
          </div>
          <label class="field">
            <span>{{ labels.workbuddyClientSecret }}</span>
            <input v-model="clientSecretDraft" type="password" placeholder="sk_live_..." />
          </label>
          <label class="field">
            <span>{{ labels.workbuddyToken }}</span>
            <input v-model="tokenDraft" type="password" placeholder="eyJhbGciOi..." />
          </label>
          <div style="display: flex; gap: 8px; flex-wrap: wrap">
            <button class="primary" @click="saveToken">{{ labels.save }}</button>
            <button @click="buildAuthUrl">{{ labels.workbuddyGetAuthUrl }}</button>
            <button @click="refreshWorkbuddy">刷新状态</button>
          </div>
          <div v-if="authUrl" class="agent-meta" style="margin-top: 8px; overflow-wrap: anywhere">
            {{ authUrl }}
          </div>
          <div class="grid-2" style="margin-top: 8px">
            <label class="field">
              <span>{{ labels.authCode }}</span>
              <input v-model="codeDraft" placeholder="auth_xxx" />
            </label>
            <div style="display: flex; align-items: flex-end">
              <button @click="exchange">{{ labels.workbuddyExchange }}</button>
            </div>
          </div>
        </div>

        <h4 style="margin: 18px 0 8px">{{ labels.theme }} / {{ labels.language }}</h4>
        <div class="grid-2">
          <div>
            <div class="composer-row">
              <button
                :class="settings.theme === 'light' ? 'primary' : ''"
                @click="emit('theme', 'light')"
              >
                {{ labels.light }}
              </button>
              <button
                :class="settings.theme === 'dark' ? 'primary' : ''"
                @click="emit('theme', 'dark')"
              >
                {{ labels.dark }}
              </button>
              <button
                :class="settings.theme === 'system' ? 'primary' : ''"
                @click="emit('theme', 'system')"
              >
                {{ labels.themeSystem }}
              </button>
            </div>
          </div>
          <div class="composer-row">
            <button
              :class="settings.locale === 'zh-CN' ? 'primary' : ''"
              @click="emit('locale', 'zh-CN')"
            >
              中文
            </button>
            <button
              :class="settings.locale === 'en' ? 'primary' : ''"
              @click="emit('locale', 'en')"
            >
              English
            </button>
          </div>
        </div>

        <div v-if="info" class="agent-meta" style="margin-top: 12px">
          <div>{{ labels.dataLocation }}: {{ info.data_dir }}</div>
          <div>protocol: {{ info.protocol }}</div>
          <div>git: {{ info.git_available ? "可用" : "不可用" }}</div>
        </div>
      </div>

      <div class="modal-foot">
        <button @click="refresh">重新检测</button>
        <button class="primary" @click="emit('close')">{{ labels.close }}</button>
      </div>
    </div>
  </div>
</template>
