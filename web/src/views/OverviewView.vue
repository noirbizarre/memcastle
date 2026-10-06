<script setup lang="ts">
import Message from "openvue/message"
import Skeleton from "openvue/skeleton"
import Tag from "openvue/tag"
import { computed } from "vue"
import ErrorNotice from "../components/ErrorNotice.vue"
import RefreshButton from "../components/RefreshButton.vue"
import PageHeader from "../components/PageHeader.vue"
import { useLoad } from "../composables/useLoad.ts"
import { duration, utc } from "../format.ts"
import { useSession } from "../session.ts"

const { client } = useSession()

// The status says how many jobs wait or run; how the finished ones ended comes from the newest of them.
const RECENT = 200
const overview = useLoad(async () => {
  const [status, jobs] = await Promise.all([client.status(), client.jobs({ limit: RECENT })])
  return { status, jobs }
}, { on: ["job", "drawer", "wing", "room"] })

const outcomes = computed(() => {
  const jobs = overview.data.value?.jobs ?? []
  const count = (state: string) => jobs.filter((job) => job.status === state).length
  return { completed: count("completed"), failed: count("failed"), cancelled: count("cancelled"), of: jobs.length }
})
const status = computed(() => overview.data.value?.status)
const healthy = computed(() => status.value?.datastore.ok && !status.value.datastore.pending.length)
</script>

<template>
  <PageHeader title="Overview" subtitle="The palace and the daemon serving it.">
    <RefreshButton :loading="overview.loading.value" :updated-at="overview.updatedAt.value" @refresh="overview.refresh()" />
  </PageHeader>
  <ErrorNotice :error="overview.error.value" @retry="overview.refresh()" />

  <Skeleton v-if="!status && !overview.error.value" height="9rem" />
  <template v-if="status">
    <section class="panel">
      <div class="row" style="margin-bottom: 1rem">
        <Tag :value="healthy ? 'running' : 'degraded'" :severity="healthy ? 'success' : 'warn'" />
        <strong>{{ status.palace_name }}</strong>
        <span class="muted">MemCastle {{ status.version }}</span>
        <Tag v-if="status.auth_enabled" value="token required" severity="secondary" />
      </div>
      <dl class="facts">
        <div><dt>Palace</dt><dd class="mono">{{ status.palace_path || "-" }}</dd></div>
        <div><dt>Listening on</dt><dd class="mono">{{ status.bind_addr || "-" }}</dd></div>
        <div><dt>Pid</dt><dd class="mono">{{ status.pid }}</dd></div>
        <div><dt>Started</dt><dd>{{ utc(status.started_at) }} <span class="muted">({{ duration(status.uptime_secs) }} ago)</span></dd></div>
        <div>
          <dt>Datastore</dt>
          <dd>{{ status.datastore.backend }} <span class="muted mono">{{ status.datastore.location }}</span></dd>
        </div>
        <div>
          <dt>Migrations</dt>
          <dd>
            {{ status.datastore.migration_version }} of {{ status.datastore.latest_version }}
            <span v-if="status.datastore.pending.length" class="muted">({{ status.datastore.pending.length }} pending)</span>
          </dd>
        </div>
      </dl>
      <Message v-if="status.datastore.error" severity="error" :closable="false">The datastore does not answer: {{ status.datastore.error }}</Message>
    </section>

    <h2>Memory</h2>
    <div class="cards">
      <div class="stat"><div class="label">Drawers</div><div class="value">{{ status.drawer_count }}</div></div>
      <div class="stat"><div class="label">Queued</div><div class="value">{{ status.jobs_queued }}</div></div>
      <div class="stat warn"><div class="label">Running</div><div class="value">{{ status.jobs_running }}</div></div>
      <div class="stat"><div class="label">Paused</div><div class="value">{{ status.jobs_paused }}</div></div>
    </div>

    <h2>Recent jobs <span class="muted" style="text-transform: none">(newest {{ outcomes.of }})</span></h2>
    <div class="cards">
      <div class="stat good"><div class="label">Completed</div><div class="value">{{ outcomes.completed }}</div></div>
      <div class="stat bad"><div class="label">Failed</div><div class="value">{{ outcomes.failed }}</div></div>
      <div class="stat"><div class="label">Cancelled</div><div class="value">{{ outcomes.cancelled }}</div></div>
    </div>
  </template>
</template>
