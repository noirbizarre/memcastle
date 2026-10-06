<script setup lang="ts">
import Button from "openvue/button"
import Message from "openvue/message"
import { useToast } from "openvue/usetoast"
import { computed, ref } from "vue"
import { RouterLink } from "vue-router"
import type { Job } from "../api/types.ts"
import ErrorNotice from "../components/ErrorNotice.vue"
import RefreshButton from "../components/RefreshButton.vue"
import PageHeader from "../components/PageHeader.vue"
import StatusTag from "../components/StatusTag.vue"
import { asApiError, useLoad } from "../composables/useLoad.ts"
import { ago, shortId } from "../format.ts"
import { useSession } from "../session.ts"

const { client, state: session } = useSession()
const toast = useToast()
const writable = computed(() => session.mode === "full")
const busy = ref<string | null>(null)

// The newest audit and repair, with their reports: what maintenance last found and did.
const latest = useLoad(async () => {
  const [audits, repairs] = await Promise.all([client.jobs({ kind: "audit", limit: 1 }), client.jobs({ kind: "repair", limit: 1 })])
  return { audit: audits[0], repair: repairs[0] }
}, { on: ["job"] })

interface Task {
  id: "audit" | "repair"
  title: string
  command: string
  description: string
}
const tasks: Task[] = [
  { id: "audit", title: "Audit", command: "memcastle job audit", description: "A read-only consistency report: orphaned drawers and dangling provenance. Changes nothing." },
  { id: "repair", title: "Repair", command: "memcastle job repair", description: "Removes what an audit finds to be orphaned. A dry run reports what it would remove and changes nothing." },
]

async function run(task: Task, dryRun: boolean): Promise<void> {
  // The one destructive action on this page: say what it does before doing it. A dry run never asks.
  if (task.id === "repair" && !dryRun && !window.confirm("Remove the orphaned drawers an audit finds? This cannot be undone. A dry run shows what would go.")) return
  busy.value = `${task.id}:${dryRun}`
  try {
    const job = await client.submitJob(task.id === "audit" ? { type: "audit" } : { type: "repair", dry_run: dryRun })
    toast.add({ severity: "success", summary: `${task.title}${dryRun && task.id === "repair" ? " (dry run)" : ""} submitted`, detail: `Job ${shortId(job.id)}`, life: 4000 })
    await latest.refresh()
  } catch (caught) {
    const error = asApiError(caught)
    toast.add({ severity: "error", summary: error.message, detail: error.help, life: 7000 })
  } finally {
    busy.value = null
  }
}

function lastJob(task: Task): Job | undefined {
  return latest.data.value?.[task.id]
}
</script>

<template>
  <PageHeader title="Maintenance" subtitle="Consistency checks on the palace. They run as jobs, with the daemon up.">
    <RefreshButton :loading="latest.loading.value" :updated-at="latest.updatedAt.value" @refresh="latest.refresh()" />
  </PageHeader>
  <Message v-if="!writable" severity="info" :closable="false">The session is read only: an audit and a dry run still work, applying a repair needs full access.</Message>
  <ErrorNotice :error="latest.error.value" @retry="latest.refresh()" />

  <div class="cards" style="grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr))">
    <section v-for="task in tasks" :key="task.id" class="panel" style="margin: 0">
      <h2 style="margin-bottom: 0.2rem">{{ task.title }}</h2>
      <code class="muted">{{ task.command }}</code>
      <p>{{ task.description }}</p>
      <div class="row">
        <Button v-if="task.id === 'repair'" label="Dry run" severity="secondary" size="small" outlined :loading="busy === 'repair:true'" @click="run(task, true)" />
        <Button
          :label="task.id === 'repair' ? 'Run' : 'Run audit'"
          :severity="task.id === 'repair' ? 'danger' : 'primary'"
          size="small"
          :disabled="task.id === 'repair' && !writable"
          :loading="busy === `${task.id}:false`"
          @click="run(task, false)"
        />
      </div>
      <div v-if="lastJob(task)" style="margin-top: 1rem">
        <div class="row">
          <span class="muted">Last run</span> <StatusTag :status="lastJob(task)!.status" />
          <span class="muted">{{ ago(lastJob(task)!.created_at) }}</span>
          <code v-if="task.id === 'repair'" class="muted">{{ lastJob(task)!.kind.dry_run ? "dry run" : "applied" }}</code>
        </div>
        <pre v-if="lastJob(task)!.result" class="drawer-content" style="margin-top: 0.6rem; max-height: 16rem">{{ JSON.stringify(lastJob(task)!.result, null, 2) }}</pre>
        <p v-else-if="lastJob(task)!.error" class="muted">{{ lastJob(task)!.error }}</p>
      </div>
      <p v-else class="muted" style="margin-top: 1rem">Never run. <RouterLink :to="{ name: 'jobs' }">See all jobs</RouterLink></p>
    </section>
  </div>
</template>
