import Aura from "@openvue/themes/aura"
import OpenVue from "openvue/config"
import ToastService from "openvue/toastservice"
import { createApp } from "vue"
import App from "./App.vue"
import { createAppRouter } from "./router.ts"
import { defaultSession, SESSION } from "./session.ts"
import "./styles.css"
import { applyTheme, watchSystemTheme } from "./theme.ts"

// Before the first paint, so a dark user never sees a light page.
applyTheme()
watchSystemTheme()

const session = defaultSession()
const app = createApp(App)
app.provide(SESSION, session)
app.use(OpenVue, { theme: { preset: Aura, options: { darkModeSelector: ".app-dark", cssLayer: false } } })
app.use(ToastService)
app.use(createAppRouter(session))
app.mount("#app")
