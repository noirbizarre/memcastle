<script setup lang="ts">
import Button from "openvue/button"
import Column from "openvue/column"
import DataTable from "openvue/datatable"
import Dialog from "openvue/dialog"
import ProgressBar from "openvue/progressbar"
import SelectButton from "openvue/selectbutton"
import { useToast } from "openvue/usetoast"
import { computed, ref, watch } from "vue"
import type { Job, JobControl, JobStatus } from "../api/types.ts"
import EmptyState from "../components/EmptyState.vue"
import ErrorNotice from "../components/ErrorNotice.vue"
import RefreshButton from "../components/RefreshButton.vue"
import PageHeader from "../components/PageHeader.vue"
import StatusTag from "../components/StatusTag.vue"
import { asApiError, useLoad, type Loaded } from "../composables/useLoad.ts"
import { ago, jobDuration, jobTarget, shortId, utc } from "../format.ts"
import { useSession } from "../session.ts"

const { client } = useSession()
const toast = useToast()

const ALL = "all"
const states = [ALL, "queued", "running", "paused", "completed", "failed", "cancelled"]
const state = ref<string>(ALL)
const kind = ref<string>(ALL)
const selected = ref<Job>()
const jobs: Loaded<Job[]> = useLoad(() => client.jobs({ status: state.value === ALL ? undefined : (state.value as JobStatus), limit: 200 }), { on: ["job"] })
watch(state, () => void jobs.refresh())
// A job that is open in the Details dialog keeps updating while it runs. If it has left the list (another state is
// filtered, say) the dialog keeps what it last showed rather than closing under the reader.
watch(jobs.data, (list) => {
  const fresh = list?.find((job) => job.id === selected.value?.id)
  if (fresh) selected.value = fresh
})

const kinds = computed(() => [ALL, ...new Set((jobs.data.value ?? []).map((job) => job.kind.type))])
const visible = computed(() => (jobs.data.value ?? []).filter((job) => kind.value === ALL || job.kind.type === kind.value))
const active = computed(() => visible.value.filter((job) => ["queued", "running", "paused"].includes(job.status)))
const history = computed(() => visible.value.filter((job) => !["queued", "running", "paused"].includes(job.status)))

/** What a job's state allows, so the buttons are the transitions the daemon would accept. */
function actions(job: Job): JobControl[] {
  switch (job.status) {
    case "queued":
      return ["cancel"]
    case "running":
      return ["pause", "cancel"]
    case "paused":
      return ["resume", "cancel"]
    case "failed":
      return ["retry"]
    default:
      return []
  }
}

async function control(job: Job, action: JobControl): Promise<void> {
  try {
    await client.controlJob(job.id, action)
    toast.add({ severity: "success", summary: `Job ${shortId(job.id)}: ${action} requested`, life: 3000 })
    await jobs.refresh()
  } catch (caught) {
    const error = asApiError(caught)
    toast.add({ severity: "error", summary: error.message, detail: error.help, life: 6000 })
  }
}

function progress(job: Job): number | undefined {
  const { current, total } = job.progress
  return total ? Math.min(100, Math.round((current / total) * 100)) : undefined
}
</script>

<template>
  <PageHeader title="Jobs" subtitle="Durable jobs and their progress. They survive a daemon restart.">
    <RefreshButton :loading="jobs.loading.value" :updated-at="jobs.updatedAt.value" @refresh="jobs.refresh()" />
  </PageHeader>
  <div class="toolbar"><span class="muted">State</span><SelectButton v-model="state" :options="states" :allow-empty="false" size="small" /></div>
  <div class="toolbar"><span class="muted">Kind</span><SelectButton v-model="kind" :options="kinds" :allow-empty="false" size="small" /></div>
  <ErrorNotice :error="jobs.error.value" @retry="jobs.refresh()" />

  <h2>Active <span class="muted">{{ active.length }}</span></h2>
  <DataTable :value="active" data-key="id" size="small" class="panel-table">
    <template #empty><EmptyState title="No active jobs" /></template>
    <Column header="ID"><template #body="{ data }"><code>{{ shortId(data.id) }}</code></template></Column>
    <Column header="Kind"><template #body="{ data }">{{ data.kind.type }}</template></Column>
    <Column header="State"><template #body="{ data }"><StatusTag :status="data.status" /></template></Column>
    <Column header="Target"><template #body="{ data }">{{ jobTarget(data.kind) }}</template></Column>
    <Column header="Progress">
      <template #body="{ data }">
        <ProgressBar v-if="progress(data) !== undefined" :value="progress(data)" style="height: 1rem" />
        <span v-else class="muted">{{ data.progress.message ?? data.progress.current }}</span>
      </template>
    </Column>
    <Column header="Created"><template #body="{ data }">{{ ago(data.created_at) }}</template></Column>
    <Column header="">
      <template #body="{ data }">
        <div class="row">
          <Button label="Details" size="small" text @click="selected = data" />
          <Button
            v-for="action in actions(data)"
            :key="action"
            :label="action"
            size="small"
            :severity="action === 'cancel' ? 'danger' : 'secondary'"
            outlined
            @click="control(data, action)"
          />
        </div>
      </template>
    </Column>
  </DataTable>

  <h2 style="margin-top: 1.5rem">History <span class="muted">{{ history.length }}</span></h2>
  <DataTable :value="history" data-key="id" size="small" paginator :rows="15" :rows-per-page-options="[15, 50, 100]">
    <template #empty><EmptyState title="No finished jobs yet" /></template>
    <Column header="ID"><template #body="{ data }"><code>{{ shortId(data.id) }}</code></template></Column>
    <Column header="Kind"><template #body="{ data }">{{ data.kind.type }}</template></Column>
    <Column header="State"><template #body="{ data }"><StatusTag :status="data.status" /></template></Column>
    <Column header="Target"><template #body="{ data }">{{ jobTarget(data.kind) }}</template></Column>
    <Column header="Created"><template #body="{ data }">{{ ago(data.created_at) }}</template></Column>
    <Column header="Duration"><template #body="{ data }">{{ jobDuration(data) }}</template></Column>
    <Column header="Attempts" field="attempt" />
    <Column header="">
      <template #body="{ data }">
        <div class="row">
          <Button label="Details" size="small" text @click="selected = data" />
          <Button v-for="action in actions(data)" :key="action" :label="action" size="small" severity="secondary" outlined @click="control(data, action)" />
        </div>
      </template>
    </Column>
  </DataTable>

  <Dialog :visible="!!selected" modal :header="selected ? `Job ${shortId(selected.id)}` : ''" :style="{ width: 'min(46rem, 95vw)' }" @update:visible="selected = undefined">
    <template v-if="selected">
      <dl class="facts" style="margin-bottom: 1rem">
        <div><dt>Id</dt><dd class="mono">{{ selected.id }}</dd></div>
        <div><dt>State</dt><dd><StatusTag :status="selected.status" /></dd></div>
        <div><dt>Requested by</dt><dd>{{ selected.requested_by }}</dd></div>
        <div><dt>Created</dt><dd>{{ utc(selected.created_at) }}</dd></div>
        <div><dt>Started</dt><dd>{{ utc(selected.started_at) }}</dd></div>
        <div><dt>Completed</dt><dd>{{ utc(selected.completed_at) }}</dd></div>
      </dl>
      <p v-if="selected.error" class="error-body"><strong>Failed:</strong> {{ selected.error }}</p>
      <h2>Parameters</h2>
      <pre class="drawer-content">{{ JSON.stringify(selected.kind, null, 2) }}</pre>
      <template v-if="selected.result">
        <h2 style="margin-top: 1rem">Result</h2>
        <pre class="drawer-content">{{ JSON.stringify(selected.result, null, 2) }}</pre>
      </template>
    </template>
  </Dialog>
</template>
