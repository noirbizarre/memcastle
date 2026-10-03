// Memory modes as a client names them, and as the daemon accepts them.
//
// The two vocabularies differ on purpose: users configure `read-only` and `off`, the daemon only accepts
// `read_only` and `disabled`. The daemon never guesses, so the translation is this file's job, and it is strict:
// `tests/fixtures/integration/modes.json` is the single source of truth and the tests hold this table to it.

/** What a user or a configuration file calls a mode. */
export type ModeLabel = "full" | "read-only" | "off"

/** What the daemon accepts in `memcastle_set_mode` and `X-MemCastle-Mode`. */
export type WireMode = "full" | "read_only" | "disabled"

const WIRE_BY_LABEL: Readonly<Record<ModeLabel, WireMode>> = {
  full: "full",
  "read-only": "read_only",
  off: "disabled",
}

export const MODE_LABELS = Object.keys(WIRE_BY_LABEL) as ModeLabel[]

/** A mode label that is not one of the three; never silently treated as `full`. */
export class InvalidModeError extends Error {
  constructor(readonly value: unknown) {
    super(
      `"${String(value)}" is not a memory mode. Use one of: ${MODE_LABELS.join(", ")}. ` +
        "MemCastle itself only accepts the wire values full, read_only and disabled, and the integration translates.",
    )
    this.name = "InvalidModeError"
  }
}

export function isModeLabel(value: unknown): value is ModeLabel {
  // `in` would accept inherited keys such as "constructor"; an own-property check cannot.
  return typeof value === "string" && Object.hasOwn(WIRE_BY_LABEL, value)
}

/** Translate a label to the wire value, refusing anything else (a wire value included) rather than guessing. */
export function toWireMode(label: unknown): WireMode {
  if (!isModeLabel(label)) throw new InvalidModeError(label)
  return WIRE_BY_LABEL[label]
}

/** The label for a wire value the daemon reported, e.g. from `memcastle_status`. */
export function toModeLabel(wire: unknown): ModeLabel {
  const found = MODE_LABELS.find((label) => WIRE_BY_LABEL[label] === wire)
  if (found === undefined) throw new InvalidModeError(wire)
  return found
}
