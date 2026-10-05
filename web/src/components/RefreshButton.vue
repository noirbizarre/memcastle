<script setup lang="ts">
import Button from "openvue/button"
import { computed } from "vue"
import Icon from "./Icon.vue"

// Re-reads what the page shows. The dashboard does not refresh by itself, so it says how old the data is.
const props = defineProps<{ loading?: boolean; updatedAt?: Date | null }>()
defineEmits<{ refresh: [] }>()

const updated = computed(() => (props.updatedAt ? `Updated ${props.updatedAt.toISOString().slice(11, 19)} UTC` : ""))
</script>

<template>
  <span class="row">
    <span v-if="updated" class="muted refresh-time">{{ updated }}</span>
    <Button severity="secondary" size="small" outlined :loading="loading" @click="$emit('refresh')">
      <Icon name="refresh" :size="15" /> Refresh
    </Button>
  </span>
</template>
