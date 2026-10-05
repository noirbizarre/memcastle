<script setup lang="ts">
import Button from "openvue/button"
import InputText from "openvue/inputtext"
import Message from "openvue/message"
import Password from "openvue/password"
import { ref } from "vue"
import { useRoute, useRouter } from "vue-router"
import brand from "@brand/icon.svg"
import { LOGIN_USER, useSession } from "../session.ts"

// The same credentials as the database console (docs/adr/015): the user `memcastle`, the daemon's token as the
// password. The token is checked by the daemon on the next request, like every other call.
const session = useSession()
const router = useRouter()
const route = useRoute()

const user = ref(LOGIN_USER)
const password = ref("")
const error = ref<string | null>(null)
const busy = ref(false)

/** Only a path inside the dashboard: `?redirect=` must not become an open redirect. */
function destination(): string {
  const wanted = route.query.redirect
  return typeof wanted === "string" && wanted.startsWith("/") && !wanted.startsWith("//") ? wanted : "/"
}

async function submit(): Promise<void> {
  busy.value = true
  error.value = null
  try {
    await session.signIn(user.value, password.value)
    password.value = ""
    await router.replace(destination())
  } catch (caught) {
    // Never echoes what was typed.
    error.value = caught instanceof Error ? caught.message : "Sign-in failed."
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="login">
    <div class="panel login-card">
      <h1><img :src="brand" alt="" width="32" height="32" /> MemCastle</h1>
      <p class="muted">This daemon requires a token. Sign in as <code>{{ LOGIN_USER }}</code> with the token as the password.</p>
      <Message v-if="session.state.notice" severity="warn" :closable="false">{{ session.state.notice }}</Message>
      <form @submit.prevent="submit">
        <div class="field">
          <label for="user">User</label>
          <InputText id="user" v-model="user" autocomplete="username" fluid />
        </div>
        <div class="field">
          <label for="password">Password (the token)</label>
          <Password v-model="password" input-id="password" :feedback="false" toggle-mask :input-props="{ autocomplete: 'current-password' }" fluid required autofocus />
        </div>
        <Message v-if="error" severity="error" :closable="false" role="alert">{{ error }}</Message>
        <Button type="submit" label="Sign in" :loading="busy" :disabled="!password" fluid />
      </form>
      <p class="muted">Generate one with <code>memcastle auth generate</code>, or use the value of <code>MEMCASTLE_AUTH_TOKEN</code>.</p>
    </div>
  </div>
</template>
