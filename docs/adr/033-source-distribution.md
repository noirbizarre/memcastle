# ADR-033: Sources are distributed as packages through static registry indexes, with one lifecycle for every origin

## Status

Accepted.
Completes [ADR-026](026-pluggable-source-adapters-as-webassembly-components.md),
which defined the package and the runtime and left distribution to this decision.

## Context

Issue #159 asks how a source is packaged, published, found, installed, updated and removed.
ADR-026 already settled the package (a gzip tarball of `memcastle-source.toml` and `source.wasm`),
the sandbox, the consent a user gives to its permissions, and install, enable, disable and remove from a file.
What was missing is everything around getting that file: a name to install by, somewhere to find it,
proof that the bytes are the ones the publisher meant, a way to take a newer version, and a way to ship official sources
without linking them into the binary.

Three constraints shape the answer.
MemCastle is a local-first daemon, so nothing may reach the network unless the user asked it to.
Installing a source decides what code the daemon runs, so it stays administrative and every new route is guarded like
the existing ones (invariant 10).
And a user must not need Rust, Python or Node to install a published source, which the component already guarantees.

## Decision

- **A registry is one static JSON file.**
  `memcastle-index.json` lists sources, and for each version its archive's location, SHA-256, the contract and MemCastle
  requirement copied from its manifest, an optional size and an optional signature, and whether it is yanked.
  Anything that serves files can host one: a repository's raw URL, static pages, a directory on a share, a mounted disk.
  There is no registry server, account or API to run.
  The format is versioned (`"format": 1`) and a number this MemCastle does not know is refused whole.
- **A registry location is an `https://` URL, `http://` to this machine only, a `file://` URL or an absolute path.**
  The same four forms are a public registry, a mirror, and offline installation, so offline is not a separate mechanism:
  it is a registry whose location is a directory.
  Relative package URLs resolve beside the index and may not climb out of it.
  A redirect is checked as the first request was.
- **The daemon fetches, and nothing else does.**
  `crate::distribution` reads indexes and archives and verifies them; `crate::app` is its only caller
  (`crate::config` only parses a location at load, and calls nothing else),
  `crate::source` (the local tooling) never opens the network, and the CLI asks the daemon over REST like every other
  command.
  `distribution` touches neither the store nor the jobs, and returns bytes without deciding to install them.
  `mining.registries` is empty by default, so a daemon reaches the network for sources only after being configured to.
- **Integrity is the index's SHA-256, always checked, before anything reads the archive.**
  The package inside must then be the one the index named: its manifest's name and version must equal the entry's,
  or an index could publish a harmless archive's digest under a name that installs something else.
  A different archive under an already published version is refused when publishing,
  because a version is what people pin.
- **Trust is a separate, optional layer: ed25519 signatures over the archive, and `mining.trust`.**
  A publisher signs with a key made by `memcastle source keygen`; a user lists the publisher's public key under
  `mining.trusted_keys`.
  With `trust = "optional"` (the default) an unsigned package installs, a signature from a key the user has not listed
  counts as unsigned, and a bad signature from a key the user *has* listed is refused as tampering.
  With `required`, only a package signed by a listed key installs from a registry, and the configuration is refused at
  load if no key is listed.
  The install records which key vouched, and `source show` prints it.
- **There are three origins and one lifecycle.**
  A built-in source is compiled in and never arrives.
  A *bundled* source is an ordinary package shipped beside the binary under `share/memcastle/sources/`, with an index
  that ships with it, and is installed by name with no registry.
  A *registry* source comes from a configured index.
  A *local* package is a file or project directory the user installed.
  They share `ProviderInfo`, the stored state, listing, enable, disable and remove;
  the stored record adds the origin, the index it came from, the archive digest and the verifying key.
  The signature policy does not apply to the bundle, whose provenance is the release that carries it,
  though its SHA-256 is checked like any other.
- **`install` takes a file, a project directory or a name.**
  A name is `claude` or `claude@1.2.0`, resolved from the first index that offers it (the bundle, then the configured
  registries in order), at the newest version that is neither yanked nor incompatible with this MemCastle.
  The compatibility test is the one the manifest and the loader use (`domain::version_compatibility`), so choosing a
  version cannot disagree with running it.
  The daemon downloads and verifies the package first and reports what it asks for, so the user agrees to the permissions
  of what was actually fetched, not of what an index claimed.
- **`update` follows the origin and never widens a source's reach silently.**
  A bundled source updates from the bundle (found by origin, so moving the installation does not strand it),
  and a registry source from the index it was installed from.
  A local package has no upstream and is never updated.
  The consent digest is unchanged (name and permissions, not version), so an update that asks for the same permissions
  needs no new agreement and one that asks for anything else comes back as "needs consent" with the digest to agree to.
  An update keeps the state the source had.
- **Packages carry their provenance and a format version.**
  The manifest gains an optional top-level `format` (absent means 1) and optional `license`, `homepage` and
  `repository` under `[source]`.
  `check_compatible` refuses a newer format with a message that says to upgrade.
- **Publishing needs no service.**
  `memcastle source package` also writes the archive's SHA-256 beside it,
  `memcastle source index` adds archives to an index (creating it, signing with `--sign`),
  and the result is uploaded anywhere.
  Both are local and need no daemon, like `init`, `build` and `test`.
- **Releases bundle `pi` and `opencode`.**
  `packaging/sources/build.sh` packages them and writes their index with the same commands a third party runs;
  the release tarballs and the `.deb` and `.rpm` put the result under `share/memcastle/sources/`.
  `directory` is the worked example of the built-in source and is not bundled, and Claude has no source yet (#88).
  Which sources ship is a line in that script and does not constrain the runtime.
- **There is no MCP tool.**
  Searching, installing and updating make the daemon fetch and run code,
  so like installing from a file they are REST (`/api/source-registry/...`) and CLI only.

## Alternatives rejected

- **A registry server with its own API, accounts and uploads.**
  It is a service to run and secure, and a hosting dependency for a local-first tool.
  A static index gets discovery, versions and verification with nothing to operate, and a server can still produce one.
- **OCI artifacts or a git repository as the registry.**
  Both need a client library or a binary and give more than an index needs.
  The index format does not prevent either from serving it later.
- **Signatures mandatory, or checksums only.**
  Mandatory signing would make a private mirror or a first-party bundle pay for a ceremony it does not need,
  and checksums alone cannot tell a compromised host from an honest one, since the index and the archives share it.
  Optional signatures with a policy let each user choose where they sit.
- **Sigstore or minisign.**
  Established, but a dependency and an online or external-tool step for a feature most users will not enable.
  ed25519 over the archive is a few lines on a crate already in the lockfile,
  and the signature field can carry another scheme if one is wanted.
- **Downloading in the CLI.**
  It would put network code and the trust policy in a client that has no business logic by design (invariant 1),
  and the daemon's own configuration is where `mining.registries` and the keys live.
- **Linking the official sources into the binary.**
  It would make them untestable as the extension model's own reference and tie their release to the core's.
  They remain independent packages that happen to ship in the same archive.
- **Treating a signature from an unknown key as a failure under `optional`.**
  A signed package would then be refused where the same package unsigned is accepted, which punishes the publisher
  who did the extra work.
  The unknown signature is simply not evidence.

## Consequences

- A user can `memcastle source search`, `install <name>` and `update` with a registry URL and nothing else,
  or install from a directory with no network.
- Publishing is `package`, `index`, upload.
  Nothing a publisher does depends on MemCastle's infrastructure.
- Trust is only as good as the keys the user lists, and a package from an unsigned registry is only as trustworthy as
  that registry's host.
  What stays true either way is that nothing runs without the user agreeing to the permissions of the exact package, and
  that a package altered after install is never run.
- A source with no installable version is still listed by `search`, with the reason, so it is not mistaken for one that
  nobody offers.
- Removing a source does not remove what it mined, as before.
- Only plain archives are supported: there is no delta, resume or mirror selection, and a download that fails is retried
  by running the command again.
