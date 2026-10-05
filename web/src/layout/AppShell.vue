<script setup lang="ts">
import Select from "openvue/select"
import { computed } from "vue"
import { RouterLink, RouterView, useRoute, useRouter } from "vue-router"
import type { MemoryMode } from "../api/types.ts"
import brand from "@brand/icon.svg"
import Icon from "../components/Icon.vue"
import ThemeToggle from "../components/ThemeToggle.vue"
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
