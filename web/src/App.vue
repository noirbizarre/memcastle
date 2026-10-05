<script setup lang="ts">
import Toast from "openvue/toast"
import { RouterView, useRoute } from "vue-router"
import AppShell from "./layout/AppShell.vue"

const route = useRoute()
</script>

<template>
  <Toast position="top-right" />
  <!-- The login page stands alone: no navigation to a page the user cannot open yet. -->
  <RouterView v-if="route.meta.public" />
  <AppShell v-else />
</template>
