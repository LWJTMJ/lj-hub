<script setup lang="ts">
interface Toast {
  id: number;
  text: string;
  kind: "info" | "error";
}

defineProps<{ items: Toast[] }>();
</script>

<template>
  <div
    v-for="item in items"
    :key="item.id"
    class="toast"
    :style="item.kind === 'error' ? 'background:#c0392b;color:#fff' : ''"
  >
    {{ item.text }}
  </div>
</template>
