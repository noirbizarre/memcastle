import { watch } from "vue"
import { createRouter, createWebHashHistory, type Router } from "vue-router"
import type { Session } from "./session.ts"

/**
 * Hash history under `/ui/`: the daemon serves a single `index.html` and needs no rewrite rule for a deep link, and a
 * reload of `/ui/#/jobs` is just `/ui/` as far as the server is concerned.
 */
export function createAppRouter(session: Session): Router {
  const router = createRouter({
    history: createWebHashHistory("/ui/"),
    routes: [
      { path: "/login", name: "login", component: () => import("./views/LoginView.vue"), meta: { public: true, title: "Sign in" } },
      { path: "/", name: "overview", component: () => import("./views/OverviewView.vue"), meta: { title: "Overview", icon: "home" } },
      { path: "/palace", name: "palace", component: () => import("./views/PalaceView.vue"), meta: { title: "Palace", icon: "building" } },
      { path: "/search", name: "search", component: () => import("./views/SearchView.vue"), meta: { title: "Search", icon: "search" } },
      { path: "/graph", name: "graph", component: () => import("./views/GraphView.vue"), meta: { title: "Graph", icon: "graph" } },
      { path: "/diary", name: "diary", component: () => import("./views/DiaryView.vue"), meta: { title: "Diary", icon: "book" } },
      { path: "/jobs", name: "jobs", component: () => import("./views/JobsView.vue"), meta: { title: "Jobs", icon: "list" } },
      { path: "/launch", name: "launch", component: () => import("./views/LaunchView.vue"), meta: { title: "Launch", icon: "play" } },
      { path: "/triggers", name: "triggers", component: () => import("./views/TriggersView.vue"), meta: { title: "Triggers", icon: "clock" } },
      { path: "/maintenance", name: "maintenance", component: () => import("./views/MaintenanceView.vue"), meta: { title: "Maintenance", icon: "wrench" } },
      { path: "/settings", name: "settings", component: () => import("./views/SettingsView.vue"), meta: { title: "Settings", icon: "settings" } },
      { path: "/:rest(.*)*", redirect: "/" },
    ],
  })

  router.beforeEach(async (to) => {
    // Asked once: whether the daemon wants a token at all, and whether the one held is accepted. A daemon that is
    // not there is not a login problem, so that error is left for the page to show.
    if (session.state.authRequired === null) {
      try {
        await session.establish()
      } catch {
        return true
      }
    }
    if (!session.state.authenticated && session.state.authRequired) {
      return to.meta.public ? true : { name: "login", query: to.fullPath === "/" ? {} : { redirect: to.fullPath } }
    }
    // Nothing to sign in to, or already signed in: the login page has nothing to offer.
    if (to.name === "login") return { name: "overview" }
    return true
  })

  // A token refused mid-session (revoked, rotated) flips `authenticated` off: go back to the login page from wherever,
  // and come back to the same view once signed in again.
  watch(
    () => session.state.authenticated,
    (authenticated) => {
      const current = router.currentRoute.value
      if (!authenticated && session.state.authRequired && !current.meta.public) {
        void router.replace({ name: "login", query: { redirect: current.fullPath } })
      }
    },
  )

  return router
}
