<script setup lang="ts">
defineProps<{ title: string; hint?: string }>()
</script>

<template>
  <div class="empty-state">
    <strong>{{ title }}</strong>
    <p v-if="hint" class="muted">{{ hint }}</p>
    <slot />
  </div>
</template>
