<script setup lang="ts">
import Message from "openvue/message"
import Column from "openvue/column"
import DataTable from "openvue/datatable"
import type { Trigger } from "../api/types.ts"
import EmptyState from "../components/EmptyState.vue"
import ErrorNotice from "../components/ErrorNotice.vue"
import PageHeader from "../components/PageHeader.vue"
import RefreshButton from "../components/RefreshButton.vue"
import StatusTag from "../components/StatusTag.vue"
import { useLoad } from "../composables/useLoad.ts"
import { ago, utc } from "../format.ts"
import { useSession } from "../session.ts"

const { client } = useSession()
// What asks for mining runs on its own, read only: a trigger is defined, enabled and fired with `memcastle trigger`,
// never from this page (docs/adr/043). A trigger fires or fails as a `trigger` event, so the page re-reads then.
const triggers = useLoad(() => client.triggers(), { on: ["trigger"] })

/** A trigger's settings on one line: `every=1d at=03:30`. */
function settings(trigger: Trigger): string {
  return Object.entries(trigger.settings ?? {})
    .map(([key, value]) => `${key}=${String(value)}`)
    .join(" ")
}
</script>

<template>
  <PageHeader title="Triggers" subtitle="What asks for a mining run on its own. Every trigger is off until you enable it.">
    <RefreshButton :loading="triggers.loading.value" :updated-at="triggers.updatedAt.value" @refresh="triggers.refresh()" />
  </PageHeader>
  <ErrorNotice :error="triggers.error.value" @retry="triggers.refresh()" />
  <Message v-if="triggers.data.value?.error" severity="warn" :closable="false">
    The configuration file cannot be read, so these are the last triggers it held: {{ triggers.data.value.error }}
  </Message>

  <section v-if="triggers.data.value" class="panel">
    <h2>Webhook listener</h2>
    <p v-if="triggers.data.value.webhook.listening">
      Listening on <code>{{ triggers.data.value.webhook.listening }}</code
      >. A sender posts to <code>/hooks/&lt;trigger&gt;</code> there, signed with that trigger's secret.
    </p>
    <p v-else-if="triggers.data.value.webhook.enabled" class="muted">Allowed, and not listening: no webhook trigger is enabled.</p>
    <p v-else class="muted">Off. Nothing listens until <code>[webhook] enable = true</code> is set and a webhook trigger is enabled.</p>
  </section>

  <EmptyState
    v-if="triggers.data.value && !triggers.data.value.triggers.length"
    title="No trigger is configured, so nothing runs unattended"
    hint="`memcastle trigger set <name> --miner <miner> --type <schedule|poll|webhook|watch>` defines one; it starts disabled."
  />
  <DataTable v-else-if="triggers.data.value" :value="triggers.data.value.triggers" data-key="name" size="small">
    <Column field="name" header="Name" />
    <Column field="miner" header="Miner" />
    <Column field="type" header="Type" />
    <Column header="Status">
      <template #body="{ data }">
        <StatusTag :status="data.status" />
        <div v-if="data.reason" class="muted">{{ data.reason }}</div>
      </template>
    </Column>
    <Column header="Settings">
      <template #body="{ data }"
        ><code>{{ settings(data) }}</code></template
      >
    </Column>
    <Column header="Last fired">
      <template #body="{ data }">
        <span :title="utc(data.last_fired_at)">{{ ago(data.last_fired_at) }}</span>
        <span class="muted"> ({{ data.fired }} run{{ data.fired === 1 ? "" : "s" }}<template v-if="data.coalesced">, {{ data.coalesced }} joined</template>)</span>
      </template>
    </Column>
    <Column header="Next due">
      <template #body="{ data }">{{ data.next_due ? utc(data.next_due) : "-" }}</template>
    </Column>
    <Column header="Last error">
      <template #body="{ data }">
        <span v-if="data.last_error" :title="utc(data.last_error_at)">{{ data.last_error }}</span>
        <span v-else class="muted">-</span>
      </template>
    </Column>
  </DataTable>
</template>
