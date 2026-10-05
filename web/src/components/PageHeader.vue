<script setup lang="ts">
defineProps<{ title: string; subtitle?: string }>()
</script>

<template>
  <header class="page-header">
    <div>
      <h1>{{ title }}</h1>
      <p v-if="subtitle" class="muted">{{ subtitle }}</p>
    </div>
    <div class="page-header-actions"><slot /></div>
  </header>
</template>
