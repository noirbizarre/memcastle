# Reference sources

One directory per source, each a complete source package (docs/writing-sources.md): a `memcastle-source.toml`, the code
that implements `wit/memcastle-source.wit`, and the conformance cases it is tested with.

| Directory | Source name | What it is |
|---|---|---|
| `directory/` | `directory-wasm` | The built-in `directory` source, written as a Rust component. The worked example. |
| `pi/` | `pi` | The Pi coding agent's session history. The official Pi source: the core has no Pi-specific code. Bundled with releases. |
| `opencode/` | `opencode` | The OpenCode coding agent's session history, acquired through its `opencode` command. Official. Bundled with releases. |
| `codex/` | `codex` | The Codex coding agent's rollout history, read from its JSONL session files. Official. Bundled with releases. |

They are maintained as examples and as the second implementation the conformance cases are run against, so that the
built-in and the WebAssembly contract cannot drift apart.
A source here is built with `memcastle source build` and tested with `memcastle source test`;
`mise run sources:check` builds and tests them all, and `mise run sources:test -- <name>` one.
A source that wraps a program ships a stand-in for it in `fixtures/bin/` (`opencode/` does), which the two tasks put first
on the `PATH`; run `memcastle source test` by hand with the same prefix.
CI runs one job per directory here, found automatically, so a new source needs no workflow change.
They are not compiled into MemCastle, and a source that ships with MemCastle's releases uses exactly the same package
contract as one a user installs.
`packaging/sources/build.sh` packages the bundled ones (`pi`, `opencode`, `claude`, `codex`, `chatgpt` and `github`) with the index that lists them, which is
what releases put under `share/memcastle/sources/`; `mise run sources:package` runs it locally, and
`docs/publishing-sources.md` is how a source gets from here to a user.
