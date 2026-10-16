<script setup lang="ts">
// Right pane: project facts, Git state, changed files, diff preview, artifacts
// and handoff status. Everything shown here is read from Git and the local
// store; nothing is estimated.

import { computed, ref, watch } from "vue";
import { api, errorText } from "../api";
import type { GitState, Project, Task } from "../types";

interface Labels {
  [key: string]: string;
}

const props = defineProps<{
  task: Task | null;
  project: Project | null;
  git: GitState | null;
  labels: Labels;
  collapsed: boolean;
}>();

const emit = defineEmits<{
  (e: "update:collapsed", value: boolean): void;
  (e: "refresh-git"): void;
  (e: "open-handoff"): void;
}>();

type Tab = "project" | "git" | "handoff";
const tab = ref<Tab>("project");
const selectedFile = ref<string | null>(null);
const diffText = ref("");
const diffError = ref("");
const busy = ref(false);

const handoff = computed(() => props.task?.handoff ?? null);

watch(
  () => props.project?.id,
  () => {
    selectedFile.value = null;
    diffText.value = "";
  },
);

async function openDiff(path: string) {
  if (!props.project) return;
  selectedFile.value = path;
  diffError.value = "";
  diffText.value = "";
  busy.value = true;
  try {
    const text = await api.gitDiff(props.project.id, path);
    if (text.trim().length === 0) {
      diffError.value = props.labels.noDiff;
    } else {
      diffText.value = text;
    }
  } catch (error) {
    diffError.value = errorText(error);
  } finally {
    busy.value = false;
  }
}

function diffLineClass(line: string): string {
  if (line.startsWith("+++") || line.startsWith("---")) return "";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "del";
  return "";
}

const dirtyFiles = computed(() => props.git?.changes ?? []);

function shortHead(head: string | null): string {
  return head ? head.slice(0, 10) : "—";
}
</script>

<template>
  <aside class="right-panel" :class="collapsed ? 'collapsed' : ''">
    <div class="panel-head">
      <button class="ghost" @click="emit('update:collapsed', !collapsed)">
        {{ collapsed ? "⇤" : "⇥" }}
      </button>
      <strong v-if="!collapsed">{{ labels.project }}</strong>
      <button
        v-if="!collapsed"
        class="ghost"
        style="margin-left: auto"
        @click="emit('refresh-git')"
      >
        ⟳
      </button>
    </div>

    <template v-if="!collapsed">
      <div class="panel-tabs">
        <button class="tab" :class="{ active: tab === 'project' }" @click="tab = 'project'">
          {{ labels.project }}
        </button>
        <button class="tab" :class="{ active: tab === 'git' }" @click="tab = 'git'">
          {{ labels.gitStatus }}
          <span v-if="git?.dirty">•</span>
        </button>
        <button class="tab" :class="{ active: tab === 'handoff' }" @click="tab = 'handoff'">
          {{ labels.handoff }}
        </button>
      </div>

      <div class="panel-body">
        <!-- project -->
        <template v-if="tab === 'project'">
          <div v-if="!project" class="hint">{{ labels.noProjects }}</div>
          <template v-else>
            <dl class="kv">
              <dt>名称</dt>
              <dd>{{ project.name }}</dd>
              <dt>路径</dt>
              <dd>{{ project.path }}</dd>
              <dt>Git 根</dt>
              <dd>{{ project.git_root ?? labels.notARepo }}</dd>
              <dt>远端</dt>
              <dd>{{ project.git_remote ?? "—" }}</dd>
            </dl>
          </template>

          <template v-if="task">
            <h4 style="margin: 14px 0 6px; font-size: 12.5px">会话</h4>
            <dl class="kv">
              <dt>Agent</dt>
              <dd>{{ task.agent_id }}</dd>
              <dt>原生会话</dt>
              <dd>{{ task.native_session_id ?? "—" }}</dd>
              <dt>恢复状态</dt>
              <dd>
                {{
                  task.native_session_restored
                    ? labels.nativeRestored
                    : labels.historyUnavailable
                }}
              </dd>
              <dt>任务状态</dt>
              <dd>{{ task.status }}</dd>
            </dl>
            <p class="hint">{{ labels.backupNote }}</p>
          </template>
        </template>

        <!-- git -->
        <template v-else-if="tab === 'git'">
          <div v-if="!git || !git.is_repo" class="hint">{{ labels.notARepo }}</div>
          <template v-else>
            <dl class="kv">
              <dt>{{ labels.branch }}</dt>
              <dd>
                {{ git.branch ?? "—" }}
                <span v-if="git.ahead || git.behind" class="hint">
                  (↑{{ git.ahead }} ↓{{ git.behind }})
                </span>
              </dd>
              <dt>{{ labels.head }}</dt>
              <dd>{{ shortHead(git.head) }}</dd>
              <dt>状态</dt>
              <dd>{{ git.dirty ? `${dirtyFiles.length} 项改动` : labels.clean }}</dd>
            </dl>

            <h4 style="margin: 12px 0 6px; font-size: 12.5px">
              {{ labels.changedFiles }}
            </h4>
            <div v-if="dirtyFiles.length === 0" class="hint">{{ labels.clean }}</div>
            <div
              v-for="change in dirtyFiles"
              :key="change.path"
              class="file-row"
              :class="{ selected: selectedFile === change.path }"
              @click="openDiff(change.path)"
            >
              <span class="file-status">{{ change.status.trim() || "M" }}</span>
              <span class="file-path">{{ change.path }}</span>
              <span v-if="!change.staged" class="badge" style="font-size: 10px">
                {{ labels.untracked }}
              </span>
            </div>

            <h4 style="margin: 14px 0 6px; font-size: 12.5px">{{ labels.diff }}</h4>
            <div v-if="busy"><span class="spinner" /></div>
            <div v-else-if="diffError" class="hint">{{ diffError }}</div>
            <pre v-else-if="diffText" class="diff"><span
              v-for="(line, index) in diffText.split('\n')"
              :key="index"
              :class="diffLineClass(line)"
            >{{ line }}
</span></pre>
            <div v-else class="hint">{{ labels.noDiff }}</div>
          </template>
        </template>

        <!-- handoff -->
        <template v-else>
          <div v-if="!handoff" class="hint">
            {{ labels.noHandoff }}
            <div style="margin-top: 8px">
              <button class="primary" :disabled="!task" @click="emit('open-handoff')">
                {{ labels.prepare }}
              </button>
            </div>
          </div>
          <template v-else>
            <dl class="kv">
              <dt>阶段</dt>
              <dd>{{ handoff.stage }}</dd>
              <dt>发送方</dt>
              <dd>{{ handoff.from_agent }}</dd>
              <dt>接收方</dt>
              <dd>{{ handoff.to_agent ?? "—" }}</dd>
              <dt>生成时间</dt>
              <dd>{{ handoff.created_at }}</dd>
              <dt>原生恢复</dt>
              <dd>{{ handoff.native_session_restored ? "是" : "否" }}</dd>
            </dl>

            <h4 style="margin: 12px 0 6px; font-size: 12.5px">
              {{ labels.gitEvidence }}
            </h4>
            <dl class="kv">
              <dt>HEAD</dt>
              <dd>{{ shortHead(handoff.git_evidence.head) }}</dd>
              <dt>工作区</dt>
              <dd>{{ handoff.git_evidence.dirty ? "有改动" : labels.clean }}</dd>
            </dl>

            <h4 style="margin: 12px 0 6px; font-size: 12.5px">{{ labels.checks }}</h4>
            <div v-if="handoff.checks.length === 0" class="hint">尚未核验</div>
            <div v-for="check in handoff.checks" :key="check.name" class="check-line">
              <span :class="check.passed ? 'badge ok' : 'badge danger'">
                {{ check.passed ? "通过" : "未通过" }}
              </span>
              <div>
                <strong>{{ check.name }}</strong>
                <div class="hint">{{ check.detail }}</div>
              </div>
            </div>

            <div style="margin-top: 12px">
              <button class="primary" @click="emit('open-handoff')">
                {{ labels.handoff }}
              </button>
            </div>
          </template>
        </template>
      </div>
    </template>
  </aside>
</template>
