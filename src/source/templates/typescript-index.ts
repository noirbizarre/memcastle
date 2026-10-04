// __NAME__: a MemCastle source, written in TypeScript and compiled to a WebAssembly component with `jco`.
//
// This one reads the `.txt` files of a flat directory, one document per file. The component is built from this module
// by `npm run build` (see package.json), which `memcastle source build` runs.
//
// Runtime constraint: TypeScript does not compile to WebAssembly directly. `jco componentize` embeds a JavaScript
// engine (StarlingMonkey) in the component, so the result is a few megabytes larger than a Rust source and starts
// slower. The host functions are the WASI ones the engine provides, plus `memcastle:source/host`.

import { readdirSync, readFileSync, statSync } from "node:fs";

const PROVIDER = "__NAME__";

type SourceRef = { provider: string; account?: string; locator: string };
type Candidate = { externalId: string; cursorAfter: string; handle: string };

// The contract's errors are WIT variants: throw `{ tag, val }` and `jco` turns it into the `source-error`.
const failure = (tag: "cursor-invalid" | "invalid-input" | "failed", val: string) => ({ tag, val });

const names = (dir: string): string[] =>
  readdirSync(dir)
    .filter((name) => name.endsWith(".txt") && statSync(`${dir}/${name}`).isFile())
    .sort();

// The cursor is `{"after": "<name>"}`: everything up to and including that name is done. `null` is the beginning.
const parseCursor = (cursor: string): string | undefined => {
  let value: unknown;
  try {
    value = JSON.parse(cursor);
  } catch {
    throw failure("cursor-invalid", "the cursor is not JSON");
  }
  if (value === null) return undefined;
  const after = (value as { after?: unknown }).after;
  if (typeof after !== "string") throw failure("cursor-invalid", "`after` is missing or not a string");
  return after;
};

// A revision must change exactly when the content does. A real source would use the provider's etag or a strong hash.
const revisionOf = (body: string): string => {
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(body)) {
    hash = ((hash ^ BigInt(byte)) * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return `${hash.toString(16).padStart(16, "0")}-${body.length}`;
};

export const adapter = {
  identify(locator?: string): SourceRef {
    if (locator === undefined) throw failure("invalid-input", "give the directory to mine");
    try {
      if (!statSync(locator).isDirectory()) throw new Error("not a directory");
    } catch {
      throw failure("invalid-input", `${locator} is not a directory this source can read`);
    }
    return { provider: PROVIDER, locator };
  },

  defaultWing(source: SourceRef): string {
    return source.locator.split("/").filter(Boolean).pop() ?? "unnamed";
  },

  defaultRoom(): string {
    return "notes";
  },

  discover(source: SourceRef, cursor: string, limit: number) {
    const after = parseCursor(cursor);
    const rest = names(source.locator).filter((name) => after === undefined || name > after);
    const candidates: Candidate[] = rest.slice(0, limit).map((name) => ({
      externalId: name,
      cursorAfter: JSON.stringify({ after: name }),
      handle: name,
    }));
    return { candidates, exhausted: rest.length <= limit };
  },

  read(source: SourceRef, candidate: Candidate) {
    const path = `${source.locator}/${candidate.handle}`;
    let body: string;
    try {
      body = readFileSync(path, "utf8");
    } catch {
      return undefined; // gone or unreadable since discovery: skipped, not an error
    }
    return {
      externalId: candidate.externalId,
      revision: revisionOf(body),
      body,
      metadata: JSON.stringify({ path }),
    };
  },

  normalize(raw: { externalId: string; body: string; metadata: string }) {
    const metadata = JSON.parse(raw.metadata) as { path?: string };
    return {
      title: raw.externalId,
      kind: "file",
      uri: metadata.path,
      tags: [] as string[],
      segments: [{ text: raw.body }],
    };
  },
};
