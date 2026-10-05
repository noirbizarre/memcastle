// A real MemCastle daemon for the tests to talk to, started the way a user starts one: `memcastle serve`.
//
// There is deliberately no fake: the point of this suite is that the dashboard's client speaks the daemon's real
// protocol, is held to its real authentication layer and sees its real errors. Modelled on the integrations' harness
// (`integrations/common/test/support/daemon.ts`), with its own copy of the discovery in `dev/daemon.ts` because `web/`
// installs on its own (docs/adr/035).

import { existsSync } from "node:fs"
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { MemCastleClient } from "../../src/api/client.ts"
import { connectable, isHealthy, registryPath } from "../../dev/daemon.ts"

type Env = Record<string, string | undefined>

/** The binary under test: `MEMCASTLE_BIN`, which `mise run web:check` sets, or a debug build in the repo. */
function binary(): string {
  const path = process.env.MEMCASTLE_BIN ?? resolve(import.meta.dir, "../../../target/debug/memcastle")
  if (!existsSync(path)) {
    // Never skip: a suite that passes because the daemon is missing proves nothing.
    throw new Error(`The memcastle binary is not at ${path}. Run \`cargo build\`, or set MEMCASTLE_BIN to a build.`)
  }
  return path
}

/** The worktree root, which is also the assets root: `web/dist` is under it. */
export const WORKTREE = resolve(import.meta.dir, "../../..")

export interface Options {
  token?: string
  /** Serve the dashboard (`web.enable`). */
  web?: boolean
  /** The assets root; the worktree by default, so the real build is what is served. */
  assets?: string
}

export class TestDaemon {
  private constructor(
    private readonly process: Bun.Subprocess<"ignore", "ignore", "pipe">,
    private readonly root: string,
    readonly baseUrl: string,
    readonly token: string | null,
  ) {}

  static async start(options: Options = {}): Promise<TestDaemon> {
    const root = await mkdtemp(join(tmpdir(), "memcastle-web-"))
    const palace = join(root, "palace")
    // Everything the daemon would read from the developer's own environment is replaced, or a local `MEMCASTLE_*`
    // variable would change what the suite tests.
    const env: Env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("MEMCASTLE_")))
    Object.assign(env, { HOME: root, XDG_STATE_HOME: join(root, "state"), XDG_DATA_HOME: join(root, "data"), XDG_CONFIG_HOME: join(root, "config") })
    if (options.token) Object.assign(env, { MEMCASTLE_AUTH_ENABLED: "true", MEMCASTLE_AUTH_TOKEN: options.token })
    if (options.web) env.MEMCASTLE_WEB_ENABLE = "true"

    const child = Bun.spawn([binary(), "serve", "--palace", palace, "--port", "0", "--assets-dir", options.assets ?? WORKTREE], {
      env,
      stdout: "ignore",
      stderr: "pipe",
    })
    const registry = await registryPath(palace, env)
    const deadline = Date.now() + 60_000
    for (;;) {
      try {
        const { bind_addr } = JSON.parse(await Bun.file(registry).text()) as { bind_addr: string }
        const baseUrl = `http://${connectable(bind_addr)}`
        if (await isHealthy(baseUrl, 500)) return new TestDaemon(child, root, baseUrl, options.token ?? null)
      } catch {
        // Not written yet.
      }
      if (child.exitCode !== null || Date.now() > deadline) {
        const stderr = await new Response(child.stderr).text().catch(() => "")
        child.kill("SIGKILL")
        await rm(root, { recursive: true, force: true })
        throw new Error(`The daemon did not start:\n${stderr}`)
      }
      await Bun.sleep(100)
    }
  }

  /** The dashboard's own client, signed in with `token` (this daemon's own when omitted). */
  client(token: string | null = this.token, mode: "full" | "read_only" = "full"): MemCastleClient {
    return new MemCastleClient({ baseUrl: this.baseUrl, token: () => token, mode: () => mode })
  }

  async stop(): Promise<void> {
    this.process.kill("SIGTERM")
    const killer = setTimeout(() => this.process.kill("SIGKILL"), 10_000)
    await this.process.exited
    clearTimeout(killer)
    await rm(this.root, { recursive: true, force: true })
  }
}

/** A directory that is an assets root with no dashboard in it: what a checkout without `mise run web:build` looks like. */
export async function emptyAssets(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "memcastle-assets-"))
  await mkdir(join(dir, "web"), { recursive: true })
  await writeFile(join(dir, "web", "README"), "no dist here")
  return dir
}
