<script setup lang="ts">
import Button from "openvue/button"
import Checkbox from "openvue/checkbox"
import InputText from "openvue/inputtext"
import Select from "openvue/select"
import Tag from "openvue/tag"
import { ref } from "vue"
import type { Ranking, SearchHit } from "../api/types.ts"
import EmptyState from "../components/EmptyState.vue"
import ErrorNotice from "../components/ErrorNotice.vue"
import PageHeader from "../components/PageHeader.vue"
import { asApiError } from "../composables/useLoad.ts"
import { shortId, utc } from "../format.ts"
import { useSession } from "../session.ts"

// What the daemon would hand an agent for a question: the hits, verbatim, with how each one ranked.
const { client } = useSession()

const q = ref("")
const wing = ref("")
const room = ref("")
const ranking = ref<Ranking>("auto")
const asOf = ref("")
const historical = ref(false)
const expand = ref(false)
const limit = ref(20)

const hits = ref<SearchHit[]>()
const error = ref<ReturnType<typeof asApiError> | null>(null)
const busy = ref(false)

async function run(): Promise<void> {
  if (!q.value.trim()) return
  busy.value = true
  error.value = null
  try {
    hits.value = await client.search({
      q: q.value.trim(),
      limit: limit.value,
      wing: wing.value.trim() || undefined,
      room: room.value.trim() || undefined,
      ranking: ranking.value,
      as_of: asOf.value.trim() || undefined,
      include_historical: historical.value || undefined,
      expand: expand.value || undefined,
    })
  } catch (caught) {
    hits.value = undefined
    error.value = asApiError(caught)
  } finally {
    busy.value = false
  }
}

function signals(hit: SearchHit): string {
  return Object.entries(hit.signals ?? {})
    .map(([leg, score]) => `${leg} ${Number(score).toFixed(2)}`)
    .join(" · ")
}
</script>

<template>
  <PageHeader title="Search" subtitle="Inspect what retrieval returns, and why." />
  <form class="panel" @submit.prevent="run">
    <div class="field">
      <label for="q">Query</label>
      <div class="row"><InputText id="q" v-model="q" placeholder="What do you want to remember?" style="flex: 1" autofocus /><Button type="submit" label="Search" :loading="busy" :disabled="!q.trim()" /></div>
    </div>
    <div class="field-grid">
      <div class="field"><label for="wing">Wing</label><InputText id="wing" v-model="wing" fluid /></div>
      <div class="field"><label for="room">Room</label><InputText id="room" v-model="room" fluid /></div>
      <div class="field"><label for="ranking">Ranking</label><Select id="ranking" v-model="ranking" :options="['auto', 'lexical', 'semantic', 'hybrid']" fluid /></div>
      <div class="field"><label for="as-of">As of (a date or an RFC 3339 instant)</label><InputText id="as-of" v-model="asOf" placeholder="2026-01-31" fluid /></div>
    </div>
    <div class="checks">
      <label><Checkbox v-model="historical" binary /> Include superseded memory</label>
      <label><Checkbox v-model="expand" binary /> Expand through the knowledge graph</label>
    </div>
  </form>

  <ErrorNotice :error="error" @retry="run" />
  <EmptyState v-if="hits && !hits.length" title="Nothing matched" hint="Try fewer words, another ranking, or widen the wing." />
  <section v-if="hits?.length" class="panel">
    <div v-for="hit in hits" :key="hit.id" class="hit">
      <div class="hit-meta">
        <Tag :value="hit.score.toFixed(3)" severity="info" />
        <code>{{ shortId(hit.id) }}</code>
        <Tag :value="hit.source.kind" severity="secondary" />
        <span v-if="hit.source.uri || hit.source.origin">{{ hit.source.uri ?? hit.source.origin?.document }}</span>
        <span>{{ utc(hit.valid_from) }}<template v-if="hit.valid_to"> → {{ utc(hit.valid_to) }}</template></span>
        <span v-if="signals(hit)">{{ signals(hit) }}</span>
        <span v-if="hit.via?.length">via {{ hit.via.join(", ") }}</span>
      </div>
      <pre>{{ hit.content.length > 700 ? `${hit.content.slice(0, 700)}…` : hit.content }}</pre>
    </div>
  </section>
</template>
