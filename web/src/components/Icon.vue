<script setup lang="ts">
// Stroke icons drawn here rather than imported: a dashboard that ships in a package should not need a font or an
// icon set from anywhere, and these few shapes are all it uses.
import { computed } from "vue"

const props = withDefaults(defineProps<{ name: string; size?: number }>(), { size: 18 })

const PATHS: Record<string, string> = {
  home: "M3 11l9-8 9 8M5 10v10h14V10",
  building: "M3 21h18M5 21V10M9 21V10M15 21V10M19 21V10M2 10l10-7 10 7z",
  search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM21 21l-5-5",
  graph:
    "M4 6a2 2 0 1 0 4 0 2 2 0 1 0-4 0M16 6a2 2 0 1 0 4 0 2 2 0 1 0-4 0M10 18a2 2 0 1 0 4 0 2 2 0 1 0-4 0M8 6h8M7 8l4 8M17 8l-4 8",
  book: "M4 5a2 2 0 0 1 2-2h13v16H6a2 2 0 0 0-2 2zM4 21V5M8 7h7",
  list: "M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01",
  play: "M6 4l14 8-14 8z",
  wrench: "M15 5a4 4 0 0 0 4 4l-9 9a2 2 0 0 1-3-3z",
  settings: "M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0M14 4v4M8 10v4M16 16v4",
  logout: "M9 4H5v16h4M16 8l4 4-4 4M20 12H9",
  sun: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4",
  moon: "M20 14a8 8 0 1 1-10-10 6 6 0 0 0 10 10z",
  monitor: "M3 4h18v12H3zM8 20h8M12 16v4",
  refresh: "M20 11a8 8 0 0 0-14-4M4 5v4h4M4 13a8 8 0 0 0 14 4M20 19v-4h-4",
  castle: "M4 21V9h2V6h2v3h2V6h4v3h2V6h2v3h2v12zM10 21v-5a2 2 0 0 1 4 0v5",
  warning: "M12 3l10 18H2zM12 10v5M12 18h.01",
  clock: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2",
  lock: "M6 11h12v10H6zM8 11V7a4 4 0 0 1 8 0v4",
}

const d = computed(() => PATHS[props.name] ?? "")
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.8"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <path :d="d" />
  </svg>
</template>
