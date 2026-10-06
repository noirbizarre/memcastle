<script setup lang="ts">
import Button from "openvue/button"
import Dialog from "openvue/dialog"
import Skeleton from "openvue/skeleton"
import Tag from "openvue/tag"
import { computed, ref, watch } from "vue"
import type { Drawer, DrawerHistory, DrawerSummary, SimilarDrawer } from "../api/types.ts"
import EmptyState from "../components/EmptyState.vue"
import ErrorNotice from "../components/ErrorNotice.vue"
import RefreshButton from "../components/RefreshButton.vue"
import PageHeader from "../components/PageHeader.vue"
import { asApiError, useLoad } from "../composables/useLoad.ts"
import { ago, shortId, utc } from "../format.ts"
import { useSession } from "../session.ts"

// Wings, rooms and drawers: browse down the hierarchy, open a drawer to read it verbatim with its history.
const { client } = useSession()

const wing = ref<string>()
const room = ref<string>()
const limit = ref(50)

// A new wing, room or drawer changes the counts as well as the lists, so all three listen to all three.
const changes = { on: ["wing", "room", "drawer"] } as const
const wings = useLoad(() => client.wings(), changes)
const detail = useLoad(async () => (wing.value ? await client.wing(wing.value) : undefined), changes)
const drawers = useLoad(async () => (wing.value && room.value ? await client.drawers(wing.value, room.value, limit.value) : undefined), changes)

watch(wing, () => {
  room.value = undefined
  limit.value = 50
  void detail.refresh()
  void drawers.refresh()
})
watch([room, limit], () => void drawers.refresh())

/** Everything the page shows, since a new drawer changes the wing's counts as well as the room's list. */
async function refreshAll(): Promise<void> {
  await Promise.all([wings.refresh(), detail.refresh(), drawers.refresh()])
}

const opened = ref<{ drawer: Drawer; history?: DrawerHistory; similar?: SimilarDrawer[] }>()
const openError = ref<ReturnType<typeof asApiError> | null>(null)

async function open(summary: DrawerSummary): Promise<void> {
  if (!wing.value || !room.value) return
  openError.value = null
  try {
    const drawer = await client.drawer(wing.value, room.value, summary.name ?? summary.id)
    opened.value = { drawer }
    // History and duplicates are extra: a failure of either must not hide the drawer.
    const [history, similar] = await Promise.allSettled([client.drawerHistory(drawer.id), client.drawerDuplicates(drawer.id)])
    opened.value = {
      drawer,
      history: history.status === "fulfilled" ? history.value : undefined,
      similar: similar.status === "fulfilled" ? similar.value : undefined,
    }
  } catch (caught) {
    openError.value = asApiError(caught)
  }
}

const canLoadMore = computed(() => (drawers.data.value?.length ?? 0) >= limit.value && limit.value < 200)
</script>

<template>
  <PageHeader title="Palace" subtitle="Wings hold rooms, rooms hold drawers: verbatim memory, as it was written.">
    <RefreshButton :loading="wings.loading.value" :updated-at="wings.updatedAt.value" @refresh="refreshAll()" />
  </PageHeader>
  <ErrorNotice :error="wings.error.value ?? detail.error.value ?? drawers.error.value ?? openError" @retry="wings.refresh()" />

  <div class="split">
    <section class="panel">
      <h2>Wings</h2>
      <Skeleton v-if="!wings.data.value && !wings.error.value" height="6rem" />
      <EmptyState v-else-if="!wings.data.value?.length" title="No wings yet" hint="Mine a directory or write a diary entry to create one." />
      <template v-for="entry in wings.data.value" :key="entry.id">
        <button type="button" class="list-item" :class="{ active: wing === entry.name }" @click="wing = entry.name">
          {{ entry.name }} <small>{{ entry.rooms }} rooms · {{ entry.drawers }} drawers</small>
        </button>
        <template v-if="wing === entry.name">
          <button
            v-for="child in detail.data.value?.rooms"
            :key="child.id"
            type="button"
            class="list-item"
            style="padding-left: 1.6rem"
            :class="{ active: room === child.name }"
            @click="room = child.name"
          >
            {{ child.name }} <small>{{ child.drawers }}</small>
          </button>
        </template>
      </template>
    </section>

    <section class="panel">
      <h2>Drawers <span v-if="wing" class="muted" style="text-transform: none">{{ wing }}<template v-if="room"> / {{ room }}</template></span></h2>
      <EmptyState v-if="!room" title="Choose a room" hint="Pick a wing, then one of its rooms." />
      <EmptyState v-else-if="drawers.data.value && !drawers.data.value.length" title="This room is empty" />
      <div v-for="entry in drawers.data.value" :key="entry.id" class="hit">
        <div class="hit-meta">
          <strong style="color: var(--p-text-color)">{{ entry.name ?? shortId(entry.id) }}</strong>
          <Tag :value="entry.source.kind" severity="secondary" />
          <span>{{ entry.chars }} characters</span>
          <span>{{ ago(entry.created_at) }}</span>
        </div>
        <div>{{ entry.preview }}</div>
        <Button label="Open" size="small" text @click="open(entry)" />
      </div>
      <Button v-if="canLoadMore" label="Load more" size="small" severity="secondary" outlined @click="limit = Math.min(200, limit + 50)" />
    </section>
  </div>

  <Dialog :visible="!!opened" modal :header="opened?.drawer.name ?? (opened ? shortId(opened.drawer.id) : '')" :style="{ width: 'min(52rem, 95vw)' }" @update:visible="opened = undefined">
    <template v-if="opened">
      <dl class="facts" style="margin-bottom: 1rem">
        <div><dt>Id</dt><dd class="mono">{{ opened.drawer.id }}</dd></div>
        <div><dt>Source</dt><dd>{{ opened.drawer.source.kind }} <span class="muted">{{ opened.drawer.source.uri ?? opened.drawer.source.origin?.document ?? "" }}</span></dd></div>
        <div><dt>Written by</dt><dd>{{ opened.drawer.source.agent ?? opened.drawer.provenance.requested_by }}</dd></div>
        <div><dt>Valid from</dt><dd>{{ utc(opened.drawer.valid_from) }}</dd></div>
        <div v-if="opened.drawer.valid_to"><dt>Valid until</dt><dd>{{ utc(opened.drawer.valid_to) }}</dd></div>
        <div v-if="opened.drawer.tags.length"><dt>Tags</dt><dd>{{ opened.drawer.tags.join(", ") }}</dd></div>
      </dl>
      <pre class="drawer-content">{{ opened.drawer.content }}</pre>
      <template v-if="opened.history && opened.history.versions.length > 1">
        <h2 style="margin-top: 1rem">History</h2>
        <div v-for="version in opened.history.versions" :key="version.id" class="hit-meta">
          <code>{{ shortId(version.id) }}</code>
          <span>{{ utc(version.valid_from) }} → {{ version.valid_to ? utc(version.valid_to) : "now" }}</span>
          <Tag v-if="version.id === opened.drawer.id" value="this version" severity="info" />
        </div>
      </template>
      <template v-if="opened.similar?.length">
        <h2 style="margin-top: 1rem">Resembles</h2>
        <div v-for="other in opened.similar" :key="other.drawer" class="hit-meta">
          <code>{{ shortId(other.drawer) }}</code><Tag :value="other.kind" severity="secondary" /><span>{{ Math.round(other.similarity * 100) }}% similar, {{ other.side }}</span>
        </div>
      </template>
    </template>
  </Dialog>
</template>
