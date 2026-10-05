<script setup lang="ts">
import Message from "openvue/message"
import Tag from "openvue/tag"
import ErrorNotice from "../components/ErrorNotice.vue"
import RefreshButton from "../components/RefreshButton.vue"
import PageHeader from "../components/PageHeader.vue"
import { useLoad } from "../composables/useLoad.ts"
import { useSession } from "../session.ts"

// What the daemon is configured to do, as it reports it: never a secret, and never read from the files.
const { client, state: session } = useSession()
const config = useLoad(() => client.config())
</script>

<template>
  <PageHeader title="Settings" subtitle="The daemon's configuration in effect. Change it in the config file or the environment, then restart.">
    <RefreshButton :loading="config.loading.value" :updated-at="config.updatedAt.value" @refresh="config.refresh()" />
  </PageHeader>
  <ErrorNotice :error="config.error.value" @retry="config.refresh()" />

  <template v-if="config.data.value">
    <Message v-if="!config.data.value.auth_enabled" severity="info" :closable="false">
      Authentication is off. Anyone who can reach <code>{{ config.data.value.bind_addr }}</code> can read and write the palace.
    </Message>
    <section class="panel">
      <h2>Daemon</h2>
      <dl class="facts">
        <div><dt>Listening on</dt><dd class="mono">{{ config.data.value.bind_addr }}</dd></div>
        <div><dt>Palace</dt><dd class="mono">{{ config.data.value.palace_path }}</dd></div>
        <div><dt>Datastore</dt><dd>{{ config.data.value.backend }} <span class="muted mono">{{ config.data.value.location }}</span></dd></div>
        <div><dt>Authentication</dt><dd><Tag :value="config.data.value.auth_enabled ? 'token required' : 'off'" :severity="config.data.value.auth_enabled ? 'success' : 'warn'" /></dd></div>
        <div><dt>Job concurrency</dt><dd>{{ config.data.value.jobs.max_concurrency }}</dd></div>
        <div><dt>Shutdown drain</dt><dd>{{ config.data.value.jobs.drain_timeout_secs }}s</dd></div>
        <div><dt>Job lease</dt><dd>{{ config.data.value.jobs.lease_ttl_secs }}s</dd></div>
      </dl>
    </section>
    <section class="panel">
      <h2>Assets and dashboard</h2>
      <dl class="facts">
        <div><dt>Assets from</dt><dd>{{ config.data.value.assets.source }} <span class="muted mono">{{ config.data.value.assets.root ?? "" }}</span></dd></div>
        <div><dt>Dashboard</dt><dd><Tag :value="config.data.value.web.built ? 'installed' : 'not installed'" :severity="config.data.value.web.built ? 'success' : 'warn'" /></dd></div>
      </dl>
    </section>
    <section class="panel">
      <h2>Providers and mining</h2>
      <dl class="facts">
        <div><dt>Embeddings</dt><dd>{{ config.data.value.embeddings.provider }} <span class="muted">{{ config.data.value.embeddings.model ?? "" }}</span></dd></div>
        <div><dt>Entity extraction</dt><dd>{{ config.data.value.extraction.provider }} <span class="muted">{{ config.data.value.extraction.model ?? "" }}</span></dd></div>
        <div><dt>Deduplication</dt><dd>{{ config.data.value.mining.dedup_enabled ? "on" : "off" }}</dd></div>
        <div><dt>Chunk size</dt><dd>{{ config.data.value.mining.chunk_chars }} characters</dd></div>
        <div><dt>Documents per mining job</dt><dd>{{ config.data.value.mining.max_documents }}</dd></div>
        <div><dt>Source registries</dt><dd>{{ config.data.value.mining.registries }}</dd></div>
      </dl>
    </section>
  </template>

  <section class="panel">
    <h2>This session</h2>
    <dl class="facts">
      <div><dt>Memory mode</dt><dd>{{ session.mode === "full" ? "full access" : "read only" }} <span class="muted">(change it in the sidebar)</span></dd></div>
    </dl>
  </section>
</template>
