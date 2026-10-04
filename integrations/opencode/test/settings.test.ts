import { expect, test } from "bun:test"
import { InvalidModeError } from "../src/modes.ts"
import { describeSettings, resolveSettings } from "../src/settings.ts"

test("without any configuration the plugin follows the daemon's own defaults", () => {
  const settings = resolveSettings(undefined, { HOME: "/home/me" })
  expect(settings).toMatchObject({
    mode: "full",
    bind: "127.0.0.1",
    port: 8420,
    palacePath: "/home/me/.local/share/memcastle/default",
    token: null,
  })
})

test("plugin options win over the environment the MemCastle CLI already reads", () => {
  const env = { MEMCASTLE_PORT: "9000", MEMCASTLE_MODE: "off", MEMCASTLE_AUTH_TOKEN: "from-env-0123456789" }
  expect(resolveSettings({ port: 9100, mode: "read-only" }, env)).toMatchObject({ port: 9100, mode: "read-only" })
  expect(resolveSettings(undefined, env)).toMatchObject({ port: 9000, mode: "off", token: "from-env-0123456789" })
})

test("a relative XDG_DATA_HOME is ignored, as the daemon ignores it, so both agree on the palace", () => {
  const settings = resolveSettings(undefined, { HOME: "/h", XDG_DATA_HOME: "relative/dir" })
  expect(settings.palacePath).toBe("/h/.local/share/memcastle/default")
})

test("an unknown mode is an error rather than full access", () => {
  expect(() => resolveSettings({ mode: "readonly" }, {})).toThrow(InvalidModeError)
  expect(() => resolveSettings(undefined, { MEMCASTLE_MODE: "disabled" })).toThrow(InvalidModeError)
})

test("a settings description for the log never contains the token", () => {
  const settings = resolveSettings({ token: "mc_super-secret-token-value" }, {})
  expect(JSON.stringify(describeSettings(settings))).not.toContain("super-secret")
})

test("the keep-alive interval defaults to a value inside the daemon's idle limit and zero turns it off", () => {
  expect(resolveSettings(undefined, {}).keepAliveMs).toBe(120_000)
  expect(resolveSettings({ keepAliveMs: 0 }, {}).keepAliveMs).toBe(0)
  expect(resolveSettings({ keepAliveMs: 30_000 }, {}).keepAliveMs).toBe(30_000)
  expect(resolveSettings({ keepAliveMs: -5 }, {}).keepAliveMs).toBe(120_000)
  expect(resolveSettings({ keepAliveMs: "soon" }, {}).keepAliveMs).toBe(120_000)
})
