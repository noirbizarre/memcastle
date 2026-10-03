// A real MemCastle daemon for the tests to talk to, started the way a user starts one: `memcastle serve`.
//
// There is deliberately no fake. The point of these tests is that the integration speaks the daemon's real protocol,
// finds it through its real registry file, and sees its real errors.

import { existsSync } from "node:fs"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { discoverDaemon } from "../../src/daemon-client.ts"
import { createSession } from "../../src/mcp-manager.ts"
import type { ModeLabel } from "../../src/modes.ts"
import { type Settings, resolveSettings } from "../../src/settings.ts"
import type { McpSession } from "../../src/persistent-mcp-client.ts"

type Env = Record<string, string | undefined>

/** The binary under test: `MEMCASTLE_BIN`, which `mise run integrations:check` sets, or a debug build in the repo. */
function binary(): string {
  const path = process.env.MEMCASTLE_BIN ?? resolve(import.meta.dir, "../../../../target/debug/memcastle")
  if (!existsSync(path)) {
    // Never skip: a suite that passes because the daemon is missing proves nothing.
    throw new Error(`The memcastle binary is not at ${path}. Run \`cargo build\`, or set MEMCASTLE_BIN to a build.`)
  }
  return path
}

export class TestDaemon {
  private constructor(
    private readonly process: Bun.Subprocess,
    private readonly root: string,
    readonly palacePath: string,
    /** The environment the *client* must use to find this daemon: only where the registry file lives. */
    readonly clientEnv: Env,
    readonly token: string | null,
  ) {}

  static async start(options: { token?: string } = {}): Promise<TestDaemon> {
    const root = await mkdtemp(join(tmpdir(), "memcastle-integration-"))
    const palacePath = join(root, "palace")
    const stateHome = join(root, "state")
    // Everything the daemon would read from the developer's own environment is replaced, or a local
    // `MEMCASTLE_*` variable would change what the suite tests.
    const env: Env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("MEMCASTLE_")))
    Object.assign(env, {
      HOME: root,
      XDG_STATE_HOME: stateHome,
      XDG_DATA_HOME: join(root, "data"),
      XDG_CONFIG_HOME: join(root, "config"),
    })
    if (options.token) Object.assign(env, { MEMCASTLE_AUTH_ENABLED: "true", MEMCASTLE_AUTH_TOKEN: options.token })

    const child = Bun.spawn([binary(), "serve", "--palace", palacePath, "--port", "0"], {
      env,
      stdout: "ignore",
      stderr: "pipe",
    })
    const clientEnv: Env = { HOME: root, XDG_STATE_HOME: stateHome }
    const daemon = new TestDaemon(child, root, palacePath, clientEnv, options.token ?? null)

    // Discovery is the code under test here too: the daemon is only "up" once the registry file names it
    // and a live health check agrees.
    const deadline = Date.now() + 60_000
    for (;;) {
      try {
        await discoverDaemon(daemon.settings({ timeoutMs: 500 }), clientEnv)
        return daemon
      } catch (error) {
        if (child.exitCode !== null || Date.now() > deadline) {
          const stderr = await new Response(child.stderr).text().catch(() => "")
          await daemon.stop()
          throw new Error(`The daemon did not start: ${String(error)}\n${stderr}`)
        }
        await Bun.sleep(100)
      }
    }
  }

  /** Settings that find this daemon the way a user's configuration would: through its palace. */
  settings(overrides: Record<string, unknown> = {}): Settings {
    return resolveSettings(
      { palacePath: this.palacePath, ...(this.token ? { token: this.token } : {}), ...overrides },
      this.clientEnv,
    )
  }

  /** A fresh, unconnected session in `mode`, as the extension would make one. */
  session(mode: ModeLabel = "full", overrides: Record<string, unknown> = {}): McpSession {
    return createSession(this.settings({ mode, ...overrides }), this.clientEnv)
  }

  async stop(): Promise<void> {
    this.process.kill("SIGTERM")
    const killer = setTimeout(() => this.process.kill("SIGKILL"), 10_000)
    await this.process.exited
    clearTimeout(killer)
    await rm(this.root, { recursive: true, force: true })
  }
}

/** Poll a job until it leaves the queue, with the same 30s ceiling the Rust suite uses. */
export async function waitForJob(session: McpSession, id: string): Promise<{ id: string; status: string; error?: string | null }> {
  const deadline = Date.now() + 30_000
  for (;;) {
    const job = await session.call<{ id: string; status: string; error?: string | null }>("memcastle_job_get", { id })
    if (["completed", "failed", "cancelled"].includes(job.status)) return job
    if (Date.now() > deadline) throw new Error(`job ${id} is still ${job.status} after 30s`)
    await Bun.sleep(100)
  }
}
