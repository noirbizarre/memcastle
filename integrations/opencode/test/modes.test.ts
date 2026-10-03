import { expect, test } from "bun:test"
import { InvalidModeError, MODE_LABELS, toModeLabel, toWireMode } from "../src/modes.ts"
import { fixture } from "./support/fixtures.ts"

const modes = fixture<{ labels: Record<string, string>; rejected_wire_values: string[] }>("modes.json")

test("every label in the shared fixture translates to the wire value the fixture names", () => {
  for (const [label, wire] of Object.entries(modes.labels)) expect(toWireMode(label)).toBe(wire as never)
})

test("the labels offered to the user are exactly the labels the shared fixture defines", () => {
  expect([...(MODE_LABELS as string[])].sort()).toEqual(Object.keys(modes.labels).sort())
})

test("a wire value or a near miss is never accepted as a label, so a typo cannot become full access", () => {
  const notLabels = [...modes.rejected_wire_values, "read_only", "disabled", "Off", "constructor", undefined, null, 3]
  for (const value of notLabels.filter((value) => !Object.hasOwn(modes.labels, value as string))) {
    expect(() => toWireMode(value)).toThrow(InvalidModeError)
  }
})

test("a wire value reported by the daemon translates back to the label the user chose", () => {
  for (const [label, wire] of Object.entries(modes.labels)) expect(toModeLabel(wire)).toBe(label as never)
  expect(() => toModeLabel("off")).toThrow(InvalidModeError)
})
