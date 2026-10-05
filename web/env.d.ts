/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue"
  const component: DefineComponent<object, object, unknown>
  export default component
}

declare module "vue-router" {
  interface RouteMeta {
    /** Reachable without signing in: the login page. */
    public?: boolean
    title?: string
    /** Present for the routes that appear in the navigation. */
    icon?: string
  }
}

export {}
