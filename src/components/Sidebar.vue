<script setup lang="ts">
import type { AgentReport, AppInfo, Project, TaskSummary } from "../types";

interface Labels {
  appName: string;
  appTagline: string;
  projects: string;
  addProject: string;
  removeProject: string;
  tasks: string;
  newTask: string;
  noProjects: string;
  noTasks: string;
  settings: string;
  agent: string;
  sessionLive: string;
  sessionOffline: string;
}

const props = defineProps<{
  runsBadge: number;
  info: AppInfo | null;
  projects: Project[];
  tasks: TaskSummary[];
  agents: AgentReport[];
  currentProjectId: string | null;
  currentTaskId: string | null;
  live: boolean;
  labels: Labels;
}>();

const emit = defineEmits<{
  (e: "add-project"): void;
  (e: "remove-project", projectId: string): void;
  (e: "select-project", projectId: string): void;
  (e: "select-task", taskId: string): void;
  (e: "create-task", agentId: string): void;
  (e: "delete-task", taskId: string): void;
  (e: "open-settings"): void;
  (e: "open-runs"): void;
}>();

function agentName(agentId: string): string {
  return props.agents.find((a) => a.id === agentId)?.display_name ?? agentId;
}

function timeLabel(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const diff = Date.now() - date.getTime();
  const minutes = Math.floor(diff / 60000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return date.toLocaleDateString();
}
</script>

<template>
  <aside class="sidebar">
    <div class="brand">
      <div class="brand-mark">LJ</div>
      <div>
        <div class="brand-name">{{ labels.appName }}</div>
        <div class="brand-tag">{{ labels.appTagline }}</div>
      </div>
    </div>

    <div class="sidebar-scroll">
      <div class="section-label">
        <span>{{ labels.projects }}</span>
        <button class="ghost" :title="labels.addProject" @click="emit('add-project')">
          +
        </button>
      </div>

      <div v-if="projects.length === 0" class="hint" style="padding: 4px 8px">
        {{ labels.noProjects }}
      </div>

      <button
        v-for="project in projects"
        :key="project.id"
        class="list-item"
        :class="{ active: project.id === currentProjectId }"
        @click="emit('select-project', project.id)"
      >
        <div class="title">{{ project.name }}</div>
        <div class="meta">
          <span v-if="project.git_root">Git</span>
          <span v-else>目录</span>
          <span>·</span>
          <span style="overflow: hidden; text-overflow: ellipsis">{{ project.path }}</span>
        </div>
      </button>

      <template v-if="currentProjectId">
        <div class="section-label">
          <span>{{ labels.tasks }}</span>
          <button
            class="ghost"
            :title="labels.newTask"
            @click="emit('create-task', agents.find((a) => a.status === 'AVAILABLE')?.id ?? agents[0]?.id ?? 'opencode')"
          >
            +
          </button>
        </div>

        <div v-if="tasks.length === 0" class="hint" style="padding: 4px 8px">
          {{ labels.noTasks }}
        </div>

        <div
          v-for="item in tasks"
          :key="item.id"
          class="list-item"
          :class="{ active: item.id === currentTaskId }"
          @click="emit('select-task', item.id)"
        >
          <div class="title">{{ item.title }}</div>
          <div class="meta">
            <span class="badge accent">{{ agentName(item.agent_id) }}</span>
            <span v-if="item.live" class="badge ok">●</span>
            <span v-if="item.has_handoff" class="badge warn">交接</span>
            <span>{{ item.message_count }} 条</span>
            <span style="margin-left: auto">{{ timeLabel(item.updated_at) }}</span>
          </div>
          <div class="meta" style="justify-content: flex-end">
            <button
              class="ghost"
              style="font-size: 11px; padding: 1px 5px"
              @click.stop="emit('delete-task', item.id)"
            >
              删除
            </button>
          </div>
        </div>
      </template>
    </div>

    <div style="padding: 8px; border-top: 1px solid var(--border)">
      <button class="ghost" style="width: 100%; text-align: left" @click="emit('open-runs')">
        🔗 协作接力
        <span v-if="runsBadge" class="badge danger" style="margin-left: 6px">
          {{ runsBadge }} 需人工
        </span>
      </button>
      <button class="ghost" style="width: 100%; text-align: left" @click="emit('open-settings')">
        ⚙ {{ labels.settings }}
      </button>
      <div v-if="info" class="hint" style="padding: 6px 6px 0">
        v{{ info.version }} · {{ info.arch }} · ACP v1
      </div>
    </div>
  </aside>
</template>
