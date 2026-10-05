// Light, dark or the system's: kept in `localStorage` (a preference, not a secret) and applied as the class the
// theme's `darkModeSelector` watches, so a reload does not flash the wrong one.

import { ref } from "vue"

export type ThemePreference = "system" | "light" | "dark"

const KEY = "memcastle.theme"
export const DARK_CLASS = "app-dark"

function read(): ThemePreference {
  const stored = typeof localStorage === "undefined" ? null : localStorage.getItem(KEY)
  return stored === "light" || stored === "dark" ? stored : "system"
}

export const preference = ref<ThemePreference>(read())

export function prefersDark(): boolean {
  return typeof matchMedia !== "undefined" && matchMedia("(prefers-color-scheme: dark)").matches
}

export function applyTheme(): void {
  const dark = preference.value === "dark" || (preference.value === "system" && prefersDark())
  document.documentElement.classList.toggle(DARK_CLASS, dark)
}

export function setTheme(next: ThemePreference): void {
  preference.value = next
  if (next === "system") localStorage.removeItem(KEY)
  else localStorage.setItem(KEY, next)
  applyTheme()
}

/** Follow the system while the preference is `system`. */
export function watchSystemTheme(): void {
  if (typeof matchMedia === "undefined") return
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (preference.value === "system") applyTheme()
  })
}
