<script setup lang="ts">
import Tag from "openvue/tag"
import { computed } from "vue"

const props = defineProps<{ status: string }>()

// One colour per state, the same everywhere a job's state is shown.
const severity = computed(() => {
  switch (props.status) {
    case "completed":
      return "success"
    case "running":
      return "info"
    case "failed":
      return "danger"
    case "paused":
      return "warn"
    case "queued":
    case "cancelled":
      return "secondary"
    default:
      return "contrast"
  }
})
</script>

<template>
  <Tag :value="status" :severity="severity" />
</template>
