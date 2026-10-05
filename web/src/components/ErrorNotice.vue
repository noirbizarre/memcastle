<script setup lang="ts">
import Button from "openvue/button"
import Message from "openvue/message"
import type { ApiError } from "../api/client.ts"

// What failed, and what to do about it: the daemon's own diagnostic, with its code for grepping.
defineProps<{ error: ApiError | null }>()
defineEmits<{ retry: [] }>()
</script>

<template>
  <Message v-if="error" severity="error" :closable="false" class="error-notice">
    <div class="error-body">
      <strong>{{ error.message }}</strong>
      <span v-if="error.help" class="muted"> {{ error.help }}</span>
      <code v-if="error.code && error.code !== 'network'" class="error-code">{{ error.code }}</code>
    </div>
    <Button label="Retry" size="small" severity="secondary" text @click="$emit('retry')" />
  </Message>
</template>
