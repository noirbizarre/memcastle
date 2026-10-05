<script setup lang="ts">
import Button from "openvue/button"
import InputText from "openvue/inputtext"
import Message from "openvue/message"
import Textarea from "openvue/textarea"
import { useToast } from "openvue/usetoast"
import { computed, ref } from "vue"
import type { Drawer } from "../api/types.ts"
import EmptyState from "../components/EmptyState.vue"
import ErrorNotice from "../components/ErrorNotice.vue"
import PageHeader from "../components/PageHeader.vue"
import { asApiError } from "../composables/useLoad.ts"
import { utc } from "../format.ts"
import { useSession } from "../session.ts"

// An agent's diary is its own account of its sessions: read by identity and wing, and written to like an agent would.
const { client, state: session } = useSession()
const toast = useToast()

const agent = ref("")
const wing = ref("")
const entries = ref<Drawer[]>()
const error = ref<ReturnType<typeof asApiError> | null>(null)
const busy = ref(false)
const entry = ref("")

const ready = computed(() => !!agent.value.trim() && !!wing.value.trim())
const writable = computed(() => session.mode === "full")

async function load(): Promise<void> {
  if (!ready.value) return
  busy.value = true
  error.value = null
  try {
    entries.value = await client.diary(agent.value.trim(), wing.value.trim(), 50)
  } catch (caught) {
    entries.value = undefined
    error.value = asApiError(caught)
  } finally {
    busy.value = false
  }
}

async function write(): Promise<void> {
  try {
    await client.writeDiary(agent.value.trim(), wing.value.trim(), entry.value.trim())
    entry.value = ""
    toast.add({ severity: "success", summary: "Diary entry written", life: 3000 })
    await load()
  } catch (caught) {
    const failure = asApiError(caught)
    toast.add({ severity: "error", summary: failure.message, detail: failure.help, life: 7000 })
  }
}
</script>

<template>
  <PageHeader title="Diary" subtitle="An agent's own notes on its sessions, newest first." />
  <form class="panel" @submit.prevent="load">
    <div class="field-grid">
      <div class="field"><label for="agent">Agent identity</label><InputText id="agent" v-model="agent" placeholder="pi" fluid /></div>
      <div class="field"><label for="wing">Wing</label><InputText id="wing" v-model="wing" placeholder="my-project" fluid /></div>
    </div>
    <Button type="submit" label="Read diary" :loading="busy" :disabled="!ready" />
  </form>
  <ErrorNotice :error="error" @retry="load" />

  <section v-if="entries" class="panel">
    <h2>Entries</h2>
    <EmptyState v-if="!entries.length" title="No entries yet" hint="This agent has not written to this wing's diary." />
    <div v-for="item in entries" :key="item.id" class="hit">
      <div class="hit-meta"><span>{{ utc(item.created_at) }}</span></div>
      <pre>{{ item.content }}</pre>
    </div>
  </section>

  <section v-if="ready" class="panel">
    <h2>Write an entry</h2>
    <Message v-if="!writable" severity="warn" :closable="false">The session is read only.</Message>
    <div class="field"><label for="entry">Entry</label><Textarea id="entry" v-model="entry" rows="4" auto-resize fluid /></div>
    <Button label="Write entry" :disabled="!writable || !entry.trim()" @click="write" />
  </section>
</template>
