<script setup lang="ts">
import Select from "openvue/select"
import { computed, onBeforeUnmount, onMounted, provide, watch } from "vue"
import { RouterLink, RouterView, useRoute, useRouter } from "vue-router"
import type { MemoryMode } from "../api/types.ts"
import brand from "@brand/icon.svg"
import Icon from "../components/Icon.vue"
import ThemeToggle from "../components/ThemeToggle.vue"
import { createEvents, EVENTS, type EventsStatus } from "../events.ts"
import { useSession } from "../session.ts"

const session = useSession()
const route = useRoute()
const router = useRouter()

const navigation = computed(() =>
  router
    .getRoutes()
    .filter((entry) => entry.meta.icon && !entry.meta.public)
    .map((entry) => ({ name: entry.name as string, path: entry.path, title: entry.meta.title as string, icon: entry.meta.icon as string })),
)

const modes: { value: MemoryMode; label: string }[] = [
  { value: "full", label: "Full access" },
  { value: "read_only", label: "Read only" },
]

// The daemon's change stream, for every page under this shell. It lives and dies with the shell, which is only
// mounted while signed in, so signing out (or a refused token) closes it. A different mode may read different things,
// so changing it opens a new stream under the new mode.
const events = createEvents(session.client)
provide(EVENTS, events)
onMounted(() => events.start())
onBeforeUnmount(() => events.stop())
watch(
  () => session.state.mode,
  () => events.start(),
)

const liveness: Record<EventsStatus, { label: string; hint: string }> = {
  live: { label: "Live", hint: "Changes appear as they happen." },
  connecting: { label: "Connecting", hint: "Opening the live updates. Use Refresh meanwhile." },
  unavailable: { label: "Manual", hint: "Live updates are not available in this session. Use Refresh." },
  off: { label: "Manual", hint: "Use Refresh to read again." },
}

async function signOut(): Promise<void> {
  session.signOut()
  await router.replace({ name: "login" })
}
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand"><img :src="brand" alt="" width="28" height="28" /><span>MemCastle</span></div>
      <nav aria-label="Main">
        <RouterLink v-for="item in navigation" :key="item.name" :to="item.path" class="nav-link" :class="{ active: route.name === item.name }">
          <Icon :name="item.icon" /> <span>{{ item.title }}</span>
        </RouterLink>
      </nav>
      <div class="sidebar-footer">
        <label class="footer-row">
          <span class="muted">Mode</span>
          <Select
            :model-value="session.state.mode"
            :options="modes"
            option-label="label"
            option-value="value"
            size="small"
            aria-label="Memory mode"
            @update:model-value="(mode: MemoryMode) => session.setMode(mode)"
          />
        </label>
        <div class="footer-row" :title="liveness[events.state.status].hint">
          <span class="muted">Updates</span>
          <span class="live" :class="events.state.status" role="status">{{ liveness[events.state.status].label }}</span>
        </div>
        <div class="footer-row">
          <span class="muted">Theme</span>
          <ThemeToggle />
        </div>
        <button v-if="session.state.authRequired" type="button" class="sign-out" @click="signOut">
          <Icon name="logout" :size="16" /> Sign out
        </button>
      </div>
    </aside>
    <main class="content">
      <RouterView />
    </main>
  </div>
</template>
