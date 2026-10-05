<script setup lang="ts">
import Button from "openvue/button"
import Checkbox from "openvue/checkbox"
import InputText from "openvue/inputtext"
import Message from "openvue/message"
import Select from "openvue/select"
import { useToast } from "openvue/usetoast"
import { computed, onMounted, ref } from "vue"
import { useRouter } from "vue-router"
import type { JobRequest } from "../api/types.ts"
import PageHeader from "../components/PageHeader.vue"
import { asApiError } from "../composables/useLoad.ts"
import { shortId } from "../format.ts"
import { useSession } from "../session.ts"

const { client, state: session } = useSession()
const toast = useToast()
const router = useRouter()

const writable = computed(() => session.mode === "full")
const busy = ref<string | null>(null)

// Mine a directory, or an installed source by name.
const mineKind = ref<"directory" | "source">("directory")
const path = ref("")
const source = ref<string | null>(null)
const locator = ref("")
const mineWing = ref("")
const full = ref(false)
const adapters = ref<string[]>([])
// A sweep over one wing, or the whole palace.
const sweepWing = ref("")

onMounted(async () => {
  try {
    adapters.value = (await client.sources()).adapters.filter((adapter) => adapter.state === "enabled").map((adapter) => adapter.name)
  } catch {
    // The directory form works without the list.
  }
})

const orUndefined = (text: string) => text.trim() || undefined

async function submit(label: string, request: JobRequest): Promise<void> {
  busy.value = label
  try {
    const job = await client.submitJob(request)
    toast.add({ severity: "success", summary: `${label} submitted`, detail: `Job ${shortId(job.id)}`, life: 4000 })
    await router.push({ name: "jobs" })
  } catch (caught) {
    const error = asApiError(caught)
    toast.add({ severity: "error", summary: error.message, detail: error.help, life: 7000 })
  } finally {
    busy.value = null
  }
}

function mine(): Promise<void> {
  const wing = orUndefined(mineWing.value)
  return submit(
    "Mine job",
    mineKind.value === "directory"
      ? { type: "mine", path: path.value.trim(), wing, full: full.value || undefined }
      : { type: "mine", source: source.value ?? "", locator: orUndefined(locator.value), wing, full: full.value || undefined },
  )
}
const canMine = computed(() => (mineKind.value === "directory" ? path.value.trim().startsWith("/") : !!source.value))
</script>

<template>
  <PageHeader title="Launch" subtitle="Jobs run in the daemon in the background. Follow them under Jobs." />
  <Message v-if="!writable" severity="warn" :closable="false">The session is read only: switch the mode to full access to submit jobs.</Message>

  <section class="panel">
    <h2>Mine</h2>
    <p class="muted">Read a directory, or an installed source, into the palace. Unchanged documents are skipped.</p>
    <div class="row" style="margin-bottom: 1rem">
      <Button label="Directory" size="small" :outlined="mineKind !== 'directory'" @click="mineKind = 'directory'" />
      <Button label="Installed source" size="small" :outlined="mineKind !== 'source'" :disabled="!adapters.length" @click="mineKind = 'source'" />
    </div>
    <div class="field-grid">
      <div v-if="mineKind === 'directory'" class="field">
        <label for="mine-path">Directory (absolute path on the daemon's machine)</label>
        <InputText id="mine-path" v-model="path" placeholder="/absolute/path" fluid />
      </div>
      <template v-else>
        <div class="field"><label for="mine-source">Source</label><Select id="mine-source" v-model="source" :options="adapters" placeholder="Choose a source" fluid /></div>
        <div class="field"><label for="mine-locator">Locator (the source's default when empty)</label><InputText id="mine-locator" v-model="locator" fluid /></div>
      </template>
      <div class="field"><label for="mine-wing">Wing (the source's default when empty)</label><InputText id="mine-wing" v-model="mineWing" fluid /></div>
    </div>
    <div class="checks"><label><Checkbox v-model="full" binary /> Read everything again, ignoring the stored cursor</label></div>
    <Button label="Submit mine job" :disabled="!writable || !canMine" :loading="busy === 'Mine job'" @click="mine" />
  </section>

  <section class="panel">
    <h2>Extract entities</h2>
    <p class="muted">Read mined drawers and add the entities and relationships they name to the knowledge graph. Needs an extraction provider.</p>
    <div class="field"><label for="extract-wing">Wing (the whole palace when empty)</label><InputText id="extract-wing" v-model="sweepWing" fluid /></div>
    <div class="row">
      <Button label="Submit extract job" :disabled="!writable" :loading="busy === 'Extract job'" @click="submit('Extract job', { type: 'extract', wing: orUndefined(sweepWing) })" />
      <Button label="Submit embed job" severity="secondary" outlined :disabled="!writable" :loading="busy === 'Embed job'" @click="submit('Embed job', { type: 'embed', wing: orUndefined(sweepWing) })" />
    </div>
    <p class="muted">The embed job computes the vectors semantic search needs, for the same wing. Needs an embeddings provider.</p>
  </section>
</template>
