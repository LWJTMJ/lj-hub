<script setup lang="ts">
// The relay control centre: one place to hand a long development task to
// several agents and watch them hand it to each other — without copying
// prompts between applications.
//
// The UI never decides anything: it shows persisted scheduler state and calls
// the same driver the automatic worker uses.

import { computed, onMounted, onUnmounted, ref } from "vue";
import { api, errorText, onRunEvent } from "../api";
import type {
  AgentReport,
  CreateRunInput,
  RelayRun,
  RelayStepStatus,
  RunDetail,
  RunSummary,
  Task,
} from "../types";

interface Labels {
  [key: string]: string;
}

const props = defineProps<{
  projectId: string | null;
  agents: AgentReport[];
  labels: Labels;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "open-task", taskId: string): void;
}>();

const runs = ref<RunSummary[]>([]);
const selectedId = ref<string | null>(null);
const detail = ref<RunDetail | null>(null);
const busy = ref(false);
const error = ref("");
const message = ref("");

// create form
const title = ref("");
const objective = ref("");
const planner = ref("");
const executor = ref("");
const reviewer = ref("");
const forbidden = ref("");
const requirementDoc = ref("USER_REQUIREMENTS.md");
const useWorktree = ref(false);
const showCreate = ref(true);
const planPreview = ref("");
const resolveNote = ref("");

let unlisten: (() => void) | null = null;

const usableAgents = computed(() => props.agents.filter((a) => a.status !== "NOT_INSTALLED"));

onMounted(async () => {
  const preferredPlanner = usableAgents.value.find((a) => a.status === "AVAILABLE");
  planner.value = preferredPlanner?.id ?? usableAgents.value[0]?.id ?? "";
  executor.value = preferredPlanner?.id ?? usableAgents.value[0]?.id ?? "";
  reviewer.value =
    preferredPlanner?.id ?? usableAgents.value[0]?.id ?? "";
  await refresh();
  unlisten = await onRunEvent(async () => {
    await refresh();
    if (selectedId.value) await open(selectedId.value, false);
  });
});

onUnmounted(() => unlisten?.());

async function refresh() {
  try {
    runs.value = (await api.listRuns()).filter(
      (r) => !props.projectId || r.project_id === props.projectId,
    );
    if (!selectedId.value && runs.value.length > 0) {
      await open(runs.value[0].id, false);
    }
  } catch (e) {
    error.value = errorText(e);
  }
}

async function open(runId: string, clearMessage = true) {
  try {
    selectedId.value = runId;
    detail.value = await api.getRun(runId);
    if (clearMessage) message.value = "";
    error.value = "";
    showCreate.value = false;
  } catch (e) {
    error.value = errorText(e);
  }
}

function stepClass(status: RelayStepStatus): string {
  switch (status) {
    case "DONE":
      return "badge ok";
    case "NEEDS_HUMAN":
      return "badge danger";
    case "FAILED":
      return "badge danger";
    case "RUNNING":
    case "VERIFYING":
    case "AWAITING_HANDOFF":
      return "badge warn";
    case "CANCELLED":
      return "badge";
    default:
      return "badge";
  }
}

async function createRun() {
  if (!props.projectId) {
    error.value = "请先选择一个项目";
    return;
  }
  if (!objective.value.trim()) {
    error.value = "请填写任务目标";
    return;
  }
  busy.value = true;
  error.value = "";
  message.value = "";
  try {
    const input: CreateRunInput = {
      project_id: props.projectId,
      title: title.value.trim() || objective.value.trim().slice(0, 40),
      objective: objective.value.trim(),
      agent: planner.value,
      requirements: [],
      forbidden: forbidden.value
        .split("\n")
        .map((l) => l.trim())
        .filter(Boolean),
      use_worktree: useWorktree.value,
      plan_json: planPreview.value.trim() ? planPreview.value : null,
    };
    const run = await api.createRun(input);
    await refresh();
    await open(run.id);
    message.value = "已创建（草稿）。下一步：生成计划。";
  } catch (e) {
    error.value = errorText(e);
  } finally {
    busy.value = false;
  }
}

async function plan() {
  if (!detail.value) return;
  busy.value = true;
  error.value = "";
  try {
    await api.planRun(detail.value.run.id);
    await open(detail.value.run.id);
    message.value = "计划已生成并通过校验。确认后即可开始自动接力。";
  } catch (e) {
    error.value = errorText(e);
  } finally {
    busy.value = false;
  }
}

async function start() {
  if (!detail.value) return;
  busy.value = true;
  error.value = "";
  try {
    await api.startRun(detail.value.run.id);
    await open(detail.value.run.id);
    message.value = "已开始自动接力：每一步都会落盘，失败会按上限自动返工。";
  } catch (e) {
    error.value = errorText(e);
  } finally {
    busy.value = false;
  }
}

async function pause() {
  if (!detail.value) return;
  try {
    await api.pauseRun(detail.value.run.id);
    await open(detail.value.run.id);
    message.value = "已暂停（当前 agent 回合不会被强杀，结束后停止推进）。";
  } catch (e) {
    error.value = errorText(e);
  }
}

async function cancel() {
  if (!detail.value) return;
  if (!window.confirm("取消这次运行？已完成的步骤记录会保留，未完成的步骤标记为已取消。")) return;
  try {
    await api.cancelRun(detail.value.run.id);
    await open(detail.value.run.id);
  } catch (e) {
    error.value = errorText(e);
  }
}

async function resolve(stepId: string, action: "retry" | "mark_done" | "cancel") {
  if (!detail.value) return;
  try {
    await api.resolveRunStep(detail.value.run.id, stepId, action, resolveNote.value);
    resolveNote.value = "";
    await open(detail.value.run.id);
    message.value = `已对该步骤执行：${action}`;
  } catch (e) {
    error.value = errorText(e);
  }
}

async function removeRun(runId: string) {
  if (!window.confirm("删除这次运行的本地记录？（不会影响项目文件与其他 Agent 的会话）")) return;
  try {
    await api.deleteRun(runId);
    if (selectedId.value === runId) {
      selectedId.value = null;
      detail.value = null;
      showCreate.value = true;
    }
    await refresh();
  } catch (e) {
    error.value = errorText(e);
  }
}

async function importRequirements() {
  if (!detail.value) return;
  try {
    const refs = await api.importRequirements(detail.value.run.id, requirementDoc.value);
    const run = detail.value.run;
    const input: CreateRunInput = {
      project_id: run.project_id,
      title: run.title,
      objective: run.objective,
      agent: run.agent,
      requirements: refs,
      forbidden: run.forbidden,
      use_worktree: false,
      plan_json: null,
    };
    // Requirements are stored on the run; re-creating is the honest way to
    // attach them without mutating an already-validated plan.
    await api.createRun(input);
    message.value = `已从 ${requirementDoc.value} 只读导入 ${refs.length} 条要求（新草稿，原文件未改动）`;
    await refresh();
  } catch (e) {
    error.value = errorText(e);
  }
}

async function copyPlan() {
  if (!detail.value) return;
  planPreview.value = JSON.stringify(detail.value.run.plan, null, 2);
  try {
    await navigator.clipboard.writeText(planPreview.value);
    message.value = "计划 JSON 已复制";
  } catch {
    message.value = "计划 JSON 已填入新建表单";
  }
  showCreate.value = true;
}

function agentName(id?: string | null): string {
  if (!id) return "—";
  return props.agents.find((a) => a.id === id)?.display_name ?? id;
}
</script>

<template>
  <div class="modal-backdrop" @click.self="emit('close')">
    <div class="modal wide">
      <div class="modal-head">
        <span class="modal-title">协作接力</span>
        <span class="badge accent">自动串行接力 · 交接核验 · 自动返工</span>
        <button class="ghost" @click="showCreate = true">＋ 新建</button>
        <button class="ghost" @click="emit('close')">关闭</button>
      </div>

      <div class="modal-body" style="display: grid; grid-template-columns: 260px 1fr; gap: 14px">
        <!-- run list -->
        <div>
          <div class="section-label" style="padding-left: 0">运行</div>
          <div v-if="runs.length === 0" class="hint">还没有接力运行。</div>
          <button
            v-for="run in runs"
            :key="run.id"
            class="list-item"
            :class="{ active: run.id === selectedId }"
            @click="open(run.id)"
          >
            <div class="title">{{ run.title }}</div>
            <div class="meta">
              <span
                class="badge"
                :class="
                  run.status === 'DONE'
                    ? 'ok'
                    : run.status === 'NEEDS_HUMAN'
                      ? 'danger'
                      : run.status === 'RUNNING'
                        ? 'warn'
                        : ''
                "
                >{{ run.status }}</span
              >
              <span>{{ run.steps_done }}/{{ run.steps_total }}</span>
              <span v-if="run.steps_needs_human" class="badge danger">需人工</span>
            </div>
          </button>
        </div>

        <!-- detail / create -->
        <div>
          <div v-if="message" class="card" style="border-color: var(--ok); margin-bottom: 10px">
            {{ message }}
          </div>
          <div
            v-if="error"
            class="card"
            style="border-color: var(--danger); color: var(--danger); margin-bottom: 10px"
          >
            {{ error }}
          </div>

          <!-- create -->
          <template v-if="showCreate">
            <h4 style="margin: 0 0 8px">新建接力任务</h4>
            <p class="hint">
              规划 Agent 会把目标拆成步骤；执行 Agent 逐步完成；验收 Agent 检查后才会进入下一步。
              验收不通过会自动生成返工任务，达到循环上限则停下等你确认。
            </p>
            <label class="field">
              <span>任务名称</span>
              <input v-model="title" placeholder="例如：实现登录并补充测试" />
            </label>
            <label class="field">
              <span>目标（写给规划 Agent 的完整需求）</span>
              <textarea v-model="objective" rows="4" placeholder="要做什么，验收标准是什么" />
            </label>
            <div class="grid-2">
              <label class="field">
                <span>规划 Agent</span>
                <select v-model="planner">
                  <option v-for="a in usableAgents" :key="a.id" :value="a.id">
                    {{ a.display_name }}（{{ a.status }}）
                  </option>
                </select>
              </label>
              <label class="field">
                <span>执行 Agent（较便宜的可用于普通编码）</span>
                <select v-model="executor" disabled>
                  <option value="">由计划决定（规划时填写）</option>
                  <option v-for="a in usableAgents" :key="a.id" :value="a.id">
                    {{ a.display_name }}
                  </option>
                </select>
              </label>
              <label class="field">
                <span>验收 Agent</span>
                <select v-model="reviewer" disabled>
                  <option value="">由计划决定（规划时填写）</option>
                </select>
              </label>
              <label class="field">
                <span>硬性禁止事项（每行一条）</span>
                <textarea v-model="forbidden" rows="2" placeholder="不要 git push" />
              </label>
            </div>
            <label class="checkline">
              <input v-model="useWorktree" type="checkbox" />
              <span>在隔离的 Git worktree 中执行（主工作区有未提交改动时会拒绝创建）</span>
            </label>
            <label class="field" style="margin-top: 10px">
              <span>可选：直接粘贴计划 JSON（留空则由规划 Agent 生成）</span>
              <textarea v-model="planPreview" rows="3" placeholder='{"phases":[...],"steps":[...]}' />
            </label>
            <div class="composer-row">
              <button class="primary" :disabled="busy" @click="createRun">创建草稿</button>
              <button v-if="detail" @click="showCreate = false">返回运行详情</button>
            </div>
          </template>

          <!-- detail -->
          <template v-else-if="detail">
            <h4 style="margin: 0 0 4px">{{ detail.run.title }}</h4>
            <div class="hint" style="margin-bottom: 8px">{{ detail.run.objective }}</div>

            <dl class="kv">
              <dt>状态</dt>
              <dd>{{ detail.run.status }}</dd>
              <dt>工作目录</dt>
              <dd>
                {{ detail.run.workspace.root || "(未设置)" }}
                <span v-if="detail.run.workspace.kind === 'worktree'" class="badge accent">
                  worktree {{ detail.run.workspace.branch }}
                </span>
              </dd>
              <dt>规划 Agent</dt>
              <dd>{{ agentName(detail.run.agent) }}</dd>
              <dt>执行次数</dt>
              <dd>
                {{ detail.run.steps.reduce((n, s) => n + s.attempts, 0) }} /
                {{ detail.run.policy.max_step_executions }}
              </dd>
              <dt v-if="detail.run.stop_reason">停止原因</dt>
              <dd v-if="detail.run.stop_reason" style="color: var(--warn)">
                {{ detail.run.stop_reason }}
              </dd>
            </dl>

            <div class="composer-row" style="margin-bottom: 10px">
              <button v-if="detail.run.plan.steps.length === 0" class="primary" :disabled="busy" @click="plan">
                生成计划
              </button>
              <button v-if="detail.run.plan.steps.length > 0" class="primary" :disabled="busy" @click="start">
                开始自动接力
              </button>
              <button :disabled="busy" @click="pause">暂停</button>
              <button :disabled="busy" @click="cancel">取消</button>
              <button @click="copyPlan">复制计划 JSON</button>
              <button @click="importRequirements">导入要求</button>
              <input v-model="requirementDoc" style="max-width: 200px" />
            </div>

            <!-- steps -->
            <div v-for="phase in detail.run.plan.phases" :key="phase.id" style="margin-bottom: 12px">
              <div class="section-label" style="padding-left: 0">
                {{ phase.title }} · 并行度 {{ phase.parallelism }} · 门 {{ phase.gate }}
              </div>
              <div
                v-for="spec in detail.run.plan.steps.filter((s) => s.phase === phase.id)"
                :key="spec.id"
                class="card"
                style="margin-bottom: 8px"
              >
                <template v-for="state in [detail.run.steps.find((s) => s.id === spec.id)]" :key="spec.id">
                  <div class="tool-head" style="background: transparent; padding: 0 0 6px">
                    <strong>{{ spec.title }}</strong>
                    <span class="tool-kind">{{ spec.id }} · {{ spec.mode }} · {{ agentName(spec.agent) }}</span>
                    <span :class="stepClass(state?.status ?? 'PENDING')" style="margin-left: auto">
                      {{ state?.status ?? "PENDING" }}
                    </span>
                  </div>
                  <div class="hint">
                    尝试 {{ state?.attempts ?? 0 }}/{{ spec.max_attempts }} ·
                    返工 {{ state?.loops ?? 0 }}/{{ spec.max_loops }} ·
                    验收：
                    {{ spec.verify_by ? agentName(spec.verify_by) : "无" }}
                    <span v-if="state?.review_status"> · 结论 {{ state.review_status }}</span>
                  </div>
                  <div v-if="state?.summary" class="tool-body" style="max-height: 120px">
                    {{ state.summary }}
                  </div>
                  <div
                    v-if="state?.needs_human_reason || state?.failure_reason"
                    class="hint warn"
                    style="margin-top: 6px"
                  >
                    {{ state.needs_human_reason || state.failure_reason }}
                  </div>
                  <div v-if="state?.findings?.length" class="hint" style="margin-top: 6px">
                    验收问题：
                    <div v-for="(f, i) in state.findings" :key="i">- {{ f }}</div>
                  </div>
                  <div class="composer-row" style="margin-top: 8px">
                    <button
                      v-if="state?.task_id"
                      class="ghost"
                      @click="emit('open-task', state.task_id as string)"
                    >
                      查看该步骤的对话记录
                    </button>
                    <template v-if="state?.status === 'NEEDS_HUMAN'">
                      <input v-model="resolveNote" placeholder="处置备注（可选）" style="max-width: 220px" />
                      <button class="primary" @click="resolve(spec.id, 'retry')">重试该步骤</button>
                      <button @click="resolve(spec.id, 'mark_done')">标记完成</button>
                      <button @click="resolve(spec.id, 'cancel')">放弃该步骤</button>
                    </template>
                  </div>
                </template>
              </div>
            </div>

            <!-- log -->
            <div class="section-label" style="padding-left: 0">运行日志</div>
            <pre class="diff" style="max-height: 200px">{{
              detail.log.length ? detail.log.join("\n") : "(暂无)"
            }}</pre>

            <div style="margin-top: 10px">
              <button class="danger" @click="removeRun(detail.run.id)">删除该运行记录</button>
            </div>
          </template>

          <div v-else class="empty">
            <p>选择左侧的运行，或新建一个接力任务。</p>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
