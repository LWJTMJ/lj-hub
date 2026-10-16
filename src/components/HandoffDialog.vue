<script setup lang="ts">
// Handoff workflow: prepare (PREPARED) → verify against the real repository
// (VERIFIED) → accept (ACCEPTED). Verification re-reads Git and the registered
// artifacts; a failed check refuses the handoff instead of proceeding.

import { computed, ref } from "vue";
import { api, errorText } from "../api";
import type {
  AgentReport,
  HandoffDraftInput,
  HandoffVerificationCheck,
  RequirementRef,
  Task,
} from "../types";

interface Labels {
  [key: string]: string;
}

const props = defineProps<{
  task: Task;
  agents: AgentReport[];
  labels: Labels;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "changed"): void;
}>();

const objective = ref(props.task.handoff?.objective ?? props.task.title);
const toAgent = ref(props.task.handoff?.to_agent ?? "");
const completed = ref((props.task.handoff?.completed ?? []).join("\n"));
const decisions = ref((props.task.handoff?.decisions ?? []).join("\n"));
const failedAttempts = ref((props.task.handoff?.failed_attempts ?? []).join("\n"));
const remaining = ref((props.task.handoff?.remaining ?? []).join("\n"));
const nextSteps = ref((props.task.handoff?.next_steps ?? []).join("\n"));
const forbidden = ref((props.task.handoff?.forbidden ?? []).join("\n"));
const registerPaths = ref("");
const requirementDoc = ref("USER_REQUIREMENTS.md");
const importedRequirements = ref<RequirementRef[]>(
  props.task.handoff?.requirements ?? [],
);
const checks = ref<HandoffVerificationCheck[]>(props.task.handoff?.checks ?? []);
const receiverAgent = ref("");
const receiverNote = ref("");
const busy = ref(false);
const message = ref("");
const error = ref("");
const markdown = ref("");

const handoff = computed(() => props.task.handoff ?? null);

function lines(value: string): string[] {
  return value
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

function draft(): HandoffDraftInput {
  return {
    to_agent: toAgent.value || null,
    objective: objective.value,
    requirements: importedRequirements.value,
    forbidden: lines(forbidden.value),
    completed: lines(completed.value),
    decisions: lines(decisions.value),
    failed_attempts: lines(failedAttempts.value),
    remaining: lines(remaining.value),
    next_steps: lines(nextSteps.value),
    artifact_ids: (props.task.handoff?.artifacts ?? []).map((a) => a.id),
    register_paths: lines(registerPaths.value),
  };
}

async function prepare() {
  busy.value = true;
  message.value = "";
  error.value = "";
  try {
    await api.prepareHandoff(props.task.id, draft());
    checks.value = await api.verifyHandoff(props.task.id);
    message.value = "已生成交接资料（PREPARED）。核验结果见下方。";
    emit("changed");
  } catch (e) {
    error.value = errorText(e);
  } finally {
    busy.value = false;
  }
}

async function verify() {
  busy.value = true;
  error.value = "";
  try {
    checks.value = await api.verifyHandoff(props.task.id);
    message.value = "已按当前仓库重新核验。";
  } catch (e) {
    error.value = errorText(e);
  } finally {
    busy.value = false;
  }
}

async function accept() {
  busy.value = true;
  error.value = "";
  message.value = "";
  try {
    const result = await api.acceptHandoff(
      props.task.id,
      receiverAgent.value || toAgent.value || "unknown",
      receiverNote.value,
    );
    message.value = result.message;
    checks.value = result.handoff.checks;
    emit("changed");
  } catch (e) {
    error.value = errorText(e);
  } finally {
    busy.value = false;
  }
}

async function importRequirements() {
  error.value = "";
  try {
    importedRequirements.value = await api.importRequirements(
      props.task.id,
      requirementDoc.value,
    );
    message.value = `从 ${requirementDoc.value} 只读导入了 ${importedRequirements.value.length} 条要求（原文件未被修改）。`;
  } catch (e) {
    error.value = errorText(e);
  }
}

async function exportMarkdown() {
  try {
    markdown.value = await api.handoffMarkdown(props.task.id);
  } catch (e) {
    error.value = errorText(e);
  }
}

async function copyMarkdown() {
  try {
    await navigator.clipboard.writeText(markdown.value);
    message.value = props.labels.copied;
  } catch (e) {
    error.value = errorText(e);
  }
}

const allPassed = computed(
  () => checks.value.length > 0 && checks.value.every((c) => c.passed),
);
</script>

<template>
  <div class="modal-backdrop" @click.self="emit('close')">
    <div class="modal">
      <div class="modal-head">
        <span class="modal-title">{{ labels.handoff }}</span>
        <span v-if="handoff" class="badge accent">{{ handoff.stage }}</span>
        <button class="ghost" @click="emit('close')">{{ labels.close }}</button>
      </div>

      <div class="modal-body">
        <div v-if="message" class="card" style="border-color: var(--ok); margin-bottom: 10px">
          {{ message }}
        </div>
        <div v-if="error" class="card" style="border-color: var(--danger); color: var(--danger); margin-bottom: 10px">
          {{ error }}
        </div>

        <label class="field">
          <span>{{ labels.objective }}</span>
          <textarea v-model="objective" rows="3" />
        </label>

        <div class="grid-2">
          <label class="field">
            <span>{{ labels.receiverAgent }}</span>
            <select v-model="toAgent">
              <option value="">(未指定)</option>
              <option v-for="agent in agents" :key="agent.id" :value="agent.id">
                {{ agent.display_name }}
              </option>
            </select>
          </label>
          <label class="field">
            <span>{{ labels.requirements }}（{{ labels.importRequirements }}）</span>
            <input v-model="requirementDoc" placeholder="USER_REQUIREMENTS.md" />
          </label>
        </div>
        <button @click="importRequirements">{{ labels.importRequirements }}</button>
        <ul v-if="importedRequirements.length" class="hint" style="margin: 8px 0">
          <li v-for="req in importedRequirements" :key="req.id">
            [{{ req.state }}] {{ req.id }} — {{ req.summary }}
          </li>
        </ul>

        <div class="grid-2" style="margin-top: 10px">
          <label class="field">
            <span>{{ labels.completed }}（{{ labels.onePerLine }}）</span>
            <textarea v-model="completed" rows="4" />
          </label>
          <label class="field">
            <span>{{ labels.decisions }}（{{ labels.onePerLine }}）</span>
            <textarea v-model="decisions" rows="4" />
          </label>
          <label class="field">
            <span>{{ labels.failedAttempts }}（{{ labels.onePerLine }}）</span>
            <textarea v-model="failedAttempts" rows="4" />
          </label>
          <label class="field">
            <span>{{ labels.remaining }}（{{ labels.onePerLine }}）</span>
            <textarea v-model="remaining" rows="4" />
          </label>
          <label class="field">
            <span>{{ labels.nextSteps }}（{{ labels.onePerLine }}）</span>
            <textarea v-model="nextSteps" rows="4" />
          </label>
          <label class="field">
            <span>{{ labels.forbidden }}（{{ labels.onePerLine }}）</span>
            <textarea v-model="forbidden" rows="4" />
          </label>
        </div>

        <label class="field">
          <span>登记重要产物（相对路径，每行一条；登记不等于备份）</span>
          <textarea v-model="registerPaths" rows="2" placeholder="dist/app.tar.gz" />
        </label>

        <div v-if="checks.length" class="card">
          <h4>{{ labels.checks }}</h4>
          <div v-for="check in checks" :key="check.name" class="check-line">
            <span :class="check.passed ? 'badge ok' : 'badge danger'">
              {{ check.passed ? "通过" : "未通过" }}
            </span>
            <div>
              <strong>{{ check.name }}</strong>
              <div class="hint">{{ check.detail }}</div>
            </div>
          </div>
        </div>

        <div class="grid-2" style="margin-top: 10px">
          <label class="field">
            <span>{{ labels.receiverAgent }}（接手确认）</span>
            <select v-model="receiverAgent">
              <option value="">(与上方相同)</option>
              <option v-for="agent in agents" :key="agent.id" :value="agent.id">
                {{ agent.display_name }}
              </option>
            </select>
          </label>
          <label class="field">
            <span>{{ labels.receiverNote }}</span>
            <input v-model="receiverNote" placeholder="已核对 HEAD / 文件 / 漂移" />
          </label>
        </div>

        <div v-if="markdown" class="card" style="margin-top: 10px">
          <h4>{{ labels.handoffMarkdown }}</h4>
          <pre class="diff" style="max-height: 260px">{{ markdown }}</pre>
          <button style="margin-top: 6px" @click="copyMarkdown">{{ labels.copied }}</button>
        </div>
      </div>

      <div class="modal-foot">
        <button @click="exportMarkdown">{{ labels.handoffMarkdown }}</button>
        <button :disabled="busy" @click="prepare">{{ labels.prepare }}</button>
        <button :disabled="busy || !handoff" @click="verify">{{ labels.verify }}</button>
        <button
          class="primary"
          :disabled="busy || !handoff"
          :title="allPassed ? '' : '核验未全部通过时会被拒绝'"
          @click="accept"
        >
          {{ labels.accept }}
        </button>
      </div>
    </div>
  </div>
</template>
