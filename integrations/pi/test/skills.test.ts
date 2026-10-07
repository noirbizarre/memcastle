// The bundled skills are offered to Pi's own skill listing, in the modes that use MemCastle and never in `off`.

import { expect, test } from "bun:test"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { skillPaths } from "../src/skills.ts"
import { SKILLS_DIR } from "../src/skill-text.ts"

const CHECKOUT = fileURLToPath(SKILLS_DIR)

test("a full session is offered the skills directory", () => {
  expect(skillPaths(CHECKOUT, { MEMCASTLE_MODE: "full" })).toEqual([CHECKOUT])
})

test("the default mode, and a read-only one, are offered the skills too", () => {
  expect(skillPaths(CHECKOUT, {})).toEqual([CHECKOUT])
  expect(skillPaths(CHECKOUT, { MEMCASTLE_MODE: "read-only" })).toEqual([CHECKOUT])
})

test("an off session is offered nothing, so it behaves as if MemCastle were not installed", () => {
  expect(skillPaths(CHECKOUT, { MEMCASTLE_MODE: "off" })).toEqual([])
})

test("a mode that does not parse offers nothing, failing closed like the rest of the extension", () => {
  expect(skillPaths(CHECKOUT, { MEMCASTLE_MODE: "everything" })).toEqual([])
})

test("a skills directory that does not exist is not offered, so Pi has nothing to report", () => {
  const dir = mkdtempSync(join(tmpdir(), "memcastle-pi-skills-"))
  try {
    expect(skillPaths(join(dir, "gone"), {})).toEqual([])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
