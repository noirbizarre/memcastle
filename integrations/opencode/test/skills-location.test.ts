// Where the shared skills are found: beside the module in an installed copy (`memcastle integration install` puts
// them there), in the repository's `skills/` in a checkout. The installed copy is a bundle, so the layout is the only
// thing it can go by.

import { expect, test } from "bun:test"
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { SKILLS_DIR } from "../src/skill-text.ts"

test("a checkout reads the repository's skills", () => {
  expect(fileURLToPath(SKILLS_DIR)).toBe(fileURLToPath(new URL("../../../skills/", import.meta.url)))
})

test("a module with a skills directory beside it reads that one, and a module without falls back to the checkout", async () => {
  const dir = mkdtempSync(join(tmpdir(), "memcastle-skills-"))
  try {
    // The module as an installed copy has it: next to a `skills/` of its own.
    cpSync(new URL("../src/skill-text.ts", import.meta.url), join(dir, "skill-text.ts"))
    mkdirSync(join(dir, "skills", "installed"), { recursive: true })
    writeFileSync(join(dir, "skills", "installed", "SKILL.md"), "---\nname: installed\n---\nthe installed text\n")

    const beside = await import(join(dir, "skill-text.ts"))

    expect(fileURLToPath(beside.SKILLS_DIR)).toBe(`${join(dir, "skills")}/`)
    expect(await beside.readSkill("installed")).toBe("the installed text")
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
