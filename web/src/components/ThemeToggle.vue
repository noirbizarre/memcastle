<script setup lang="ts">
import { preference, setTheme, type ThemePreference } from "../theme.ts"
import Icon from "./Icon.vue"

const choices: { value: ThemePreference; icon: string; label: string }[] = [
  { value: "system", icon: "monitor", label: "Follow the system theme" },
  { value: "light", icon: "sun", label: "Light theme" },
  { value: "dark", icon: "moon", label: "Dark theme" },
]
</script>

<template>
  <div class="theme-toggle" role="group" aria-label="Theme">
    <button
      v-for="choice in choices"
      :key="choice.value"
      type="button"
      :class="{ active: preference === choice.value }"
      :aria-label="choice.label"
      :aria-pressed="preference === choice.value"
      :title="choice.label"
      @click="setTheme(choice.value)"
    >
      <Icon :name="choice.icon" :size="14" />
    </button>
  </div>
</template>
