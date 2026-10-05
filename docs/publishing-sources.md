# Publishing and installing sources

A mining source is a [WebAssembly package](writing-sources.md), and this page is how one gets from its author to a user:
the package format, the registry index that lists packages, how to publish to one, and how users find, install, update
and trust sources.
The decisions behind it are in [ADR-033](adr/033-source-distribution.md).

Nobody installing a published source needs Rust, Python, Node or an SDK: a package is a component and a manifest, and the
daemon runs it.

## Where a source comes from

| Origin | What it is | Installed by |
|---|---|---|
| Built in | Compiled into MemCastle (`directory`). | Nothing: it is always there and cannot be removed. |
| Bundled | An ordinary package shipped beside the binary, with an index. | `memcastle source install <name>`, with no registry and no network. |
| Registry | A package listed in an index you configured. | `memcastle source install <name>`. |
| Local | A package file or a project directory on your disk. | `memcastle source install <file or directory>`. |

All of them share one lifecycle: they are listed together, and an installed one is enabled, disabled and removed the same
way.
The origin only decides what `update` looks at.

## The package format

A package is a gzip-compressed tar archive named `<name>-<version>.tar.gz`, written by `memcastle source package`.

| File | Required | What it is |
|---|---|---|
| `memcastle-source.toml` | yes | The [manifest](writing-sources.md#the-manifest), exactly as written. |
| `source.wasm` | yes | The WebAssembly component. A plain core module is refused. |
| `README.md`, `LICENSE`, `LICENSE.md` | no | Kept beside the component for whoever installed it. |

Only those top-level files are ever read, whatever else an archive holds, and one may expand to at most 256 MiB.
The archive is deterministic: packaging the same source twice gives the same bytes, so its SHA-256 can be published.

What the manifest says about the package itself:

| Field | Meaning |
|---|---|
| `format` | The manifest format, 1 when absent. A manifest in a format this MemCastle does not know is `incompatible`, so a field that means something new is never half-understood. |
| `source.name`, `source.version` | The identity: the name users install by and the semantic version. |
| `source.description`, `source.license`, `source.homepage`, `source.repository` | What it reads, and the provenance people choose by. The last three are optional. |
| `compatibility.contract` | The WIT contract version it was built against. |
| `compatibility.memcastle` | The MemCastle versions it runs on, as a semver requirement. |
| `capabilities`, `permissions`, `limits` | What it can do and what it asks the host for. |

**Compatibility** is checked at install, every time the source is loaded, and for every version an index offers,
always by the same rule: the contract must be one this MemCastle implements, and the requirement must match this MemCastle's
release (a pre-release is held to its release's requirement).
A source that stops fitting after an upgrade is `unavailable` with the reason, never a mysterious failure.

**Integrity** has two parts.
The component's SHA-256 is recorded at install and checked every time it is loaded, so an altered file is never run.
The archive's SHA-256, which a registry index pins, is checked before anything is read from the archive.

## The registry index

A registry is one JSON file, `memcastle-index.json`, served from anywhere that serves files.

```json
{
  "format": 1,
  "name": "My registry",
  "sources": [
    {
      "name": "claude",
      "description": "Claude Code session history",
      "license": "MIT",
      "homepage": "https://example.org/claude",
      "versions": [
        {
          "version": "1.2.0",
          "contract": "0.1",
          "memcastle": ">=0.2, <0.4",
          "url": "claude-1.2.0.tar.gz",
          "sha256": "<64 lowercase hex characters>",
          "size": 123456,
          "signature": { "key": "<key id>", "value": "<base64>" },
          "yanked": false
        }
      ]
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `format` | The index format, 1. A higher number is refused. |
| `sources[].name` | A [source name](writing-sources.md#the-manifest), unique in the index. |
| `versions[].version` | A semantic version, unique for the source. |
| `versions[].contract`, `memcastle` | Copied from the package's manifest, so a version that cannot run here is skipped without downloading it. |
| `versions[].url` | Where the archive is: absolute, or relative to the index's own location. A relative URL may not climb out of the index's directory. |
| `versions[].sha256` | SHA-256 of the archive. Always checked. |
| `versions[].size` | Optional, informational. |
| `versions[].signature` | Optional: an ed25519 signature over the archive's bytes, and the id of the key. |
| `versions[].yanked` | Withdrawn: never chosen as the newest, but still installable by asking for exactly that version. |

An index is at most 4 MiB and an archive at most 64 MiB.

**Where an index can be** is the same for the bundle, a public registry, a mirror and an offline copy:

| Location | Example |
|---|---|
| `https://` URL | `https://example.org/memcastle/memcastle-index.json` (a URL ending in `/` means the index file inside it) |
| `http://` URL, to this machine only | `http://localhost:8000/memcastle-index.json` |
| `file://` URL | `file:///srv/sources/memcastle-index.json` |
| An absolute path | `/srv/sources` (a directory means the index file inside it) |

Plain `http://` to anything beyond this machine is refused, and so is a redirect that leaves the allowed places, because
the index has no digest of its own to catch tampering in transit.

**Choosing a version** is the same for `install`, `search` and `update`: the highest semantic version that is not yanked
and runs on this MemCastle.
A name pinned as `claude@1.1.0` takes exactly that version, or says why it cannot.
The first index that offers a name decides, in the order: the bundle, then `mining.registries` as listed.

## Publishing

Nothing here needs a service or an account: a registry is a directory of archives and one index file.

1. **Scaffold, build and test**: `memcastle source init`, `memcastle source build`, `memcastle source test`.
2. **Bump `source.version`** in `memcastle-source.toml` for a new release.
   A version never changes once published.
3. **Package**: `memcastle source package` writes `dist/<name>-<version>.tar.gz` and `dist/<name>-<version>.tar.gz.sha256`
   (the format `sha256sum -c` reads), and prints the archive's digest and the permissions it asks for.
4. **Sign** (optional, once): `memcastle source keygen publisher.key` writes a private key, readable by you alone, and
   prints its id and the public key.
   Keep the file private, and publish the public key where your users can find it.
5. **Add to the index**:

   ```sh
   memcastle source index dist/claude-1.2.0.tar.gz \
       --output registry/memcastle-index.json --base-url https://example.org/memcastle --sign publisher.key
   ```

   The index is created or extended, versions are listed newest first, and sources by name.
   Without `--base-url` the package URLs are relative, so the archives must sit beside the index.
   Publishing the same archive again refreshes its entry and keeps a withdrawal; a different archive under a version that
   is already listed is refused.
6. **Upload** the archive and the index anywhere that serves files.
   To withdraw a bad release, set `"yanked": true` on its entry.

Users then add the registry to their configuration:

```toml
[mining]
registries = ["https://example.org/memcastle/memcastle-index.json"]
```

## Installing, searching and updating

```sh
memcastle source search claude                  # what the bundle and the registries offer
memcastle source install claude                 # newest version that runs here, after you agree to its permissions
memcastle source install claude@1.1.0 --enable
memcastle source install ./claude-1.2.0.tar.gz  # a file: no registry involved
memcastle source install ./my-source            # a project directory: built, packaged, then installed
memcastle source update --check                 # which installed sources have a newer version
memcastle source update                         # update them
```

The daemon does the fetching, so `mining.registries` and the trust settings are the daemon's configuration, and
`--registry <LOCATION>` consults one place only for a single command.
Before anything is installed the daemon has downloaded and verified the package, and the CLI shows what that package
asks for: you agree to the permissions of what was actually fetched.
`source show` afterwards says where it came from, which registry and, when there is one, which key vouched for it.

**Updating** installs the newest version from the same origin: the bundle for a bundled source, the index it was installed
from for a registry source.
A source you installed from a file has no upstream and is left alone.
An update keeps the source's state.
The consent you gave is to a set of permissions, not to a version, so an update that asks for the same permissions needs
no new agreement, and one that asks for anything else is not installed until you agree to exactly those: the command says
what is waiting and exits non-zero, and `--yes` (or the prompt) agrees.
A source cannot widen its own reach by being updated.

**Offline.** A registry that is a directory needs no network, and a package file installs with none either:

```sh
memcastle source install ./claude-1.2.0.tar.gz --yes
memcastle source install claude --registry /mnt/usb/sources
```

**Removing** a source deletes its files and its record; what it mined stays in the palace.

## Trust

Two layers, and only the second is optional.

1. **Integrity**: the archive's SHA-256 must equal the one the index published, and the package inside must carry the name
   and version the index lists.
   This catches a corrupted download and an archive swapped for another.
   It cannot catch a compromised host, because the index and the archives usually live in the same place.
2. **Trust**: an ed25519 signature, made with the publisher's key, over the archive, checked against the public keys you
   list.

```toml
[mining]
trust = "optional"    # or "required"
trusted_keys = ["<the base64 public key `memcastle source keygen` printed>"]
```

| `trust` | An unsigned package | Signed by a key you list | Signed by a key you do not list |
|---|---|---|---|
| `optional` (default) | installs | installs if the signature is valid, refused if it is not | installs, as if unsigned |
| `required` | refused | installs if the signature is valid, refused if it is not | refused |

A bad signature from a key you trust is refused even under `optional`: it is evidence of tampering, not an absence of a
signature.
`required` needs at least one key listed, and the configuration is refused at load otherwise.
The sources bundled with MemCastle are not subject to the signature policy: their provenance is the release that carries
them, and their SHA-256 is checked like any other.

Whatever the policy, nothing runs without your agreement to the permissions of the exact package, and a package altered
on disk after install is never run.

## Bundled sources

Releases ship `pi` and `opencode` as packages, with an index, under `share/memcastle/sources/`:
in the release tarballs, and as `/usr/share/memcastle/sources/` in the `.deb` and `.rpm`.
The daemon finds them beside the binary (the same search as other [runtime assets](configuration.md#runtime-assets)),
or in `mining.bundled_dir`, so `memcastle source install pi` needs no registry and no network.
They are not linked into the binary and are installed, updated and removed like any other source.
A standalone binary carries none: install from a file or a registry instead.

The set is a line in `packaging/sources/build.sh`, which a release and `mise run sources:package` both run,
and it does not constrain the runtime: any directory under `sources/` could be added.
`directory` is the worked example of the built-in source and is not bundled.

## When something goes wrong

| Code | Meaning |
|---|---|
| `memcastle::source::registry_unavailable` | An index could not be read. The error names the location. |
| `memcastle::source::not_in_registry` | No index offers the name, or none of its versions runs here; the error lists what exists. |
| `memcastle::source::integrity` | The download is not what was published. Nothing was installed. |
| `memcastle::source::untrusted` | The trust policy refuses the package. |
| `memcastle::source::consent_required` | The package asks for permissions you have not agreed to. |

The full list is in [Writing a mining source](writing-sources.md#troubleshooting).
