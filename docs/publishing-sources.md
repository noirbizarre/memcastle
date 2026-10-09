# Publishing and installing sources

A mining source is a [WebAssembly package](writing-sources.md), and this page is how one gets from its author to a user:
the package format, the registry index that lists packages, how to publish to one, and how users find, install, update
and trust sources.
The decisions behind it are in [ADR-033](adr/033-source-distribution.md)
and [ADR-040](adr/040-bundled-sources-are-installed-from-the-start-and-the-official-registry-is-published.md).

Nobody installing a published source needs Rust, Python, Node or an SDK: a package is a component and a manifest, and the
daemon runs it.

## Where a source comes from

| Origin | What it is | Installed by |
|---|---|---|
| Built in | Compiled into MemCastle (`directory`). | Nothing: it is always there and cannot be removed. |
| Bundled | An ordinary package unpacked beside the binary (`pi`, `opencode`, `claude`, `codex`, `chatgpt`, `github`). | Nothing: it is installed from the start, and `memcastle source enable <name>` turns it on. |
| Registry | A package listed in an index you configured. | `memcastle source install <name>`. |
| Local | A package file or a project directory on your disk. | `memcastle source install <file or directory>`. |

They are listed together and share one lifecycle, and `install` and `update` are for the last two:
a bundled source is already installed and is updated with MemCastle, and a built-in one is MemCastle.
The origin decides what `update` looks at.

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
          "contract": "0.4",
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
| `repository` | Instead of `versions`: a GitHub repository, `owner/name`, whose [releases](#registering-a-github-repository) publish the source. |
| `versions[].contract`, `memcastle` | Optional. Copied from the package's manifest, so a version that cannot run here is skipped without downloading it. |
| `versions[].url` | Where the archive is: absolute, or relative to the index's own location. A relative URL may not climb out of the index's directory. |
| `versions[].sha256` | SHA-256 of the archive. Always checked. |
| `versions[].size` | Optional, informational. |
| `versions[].signature` | Optional: an ed25519 signature over the archive's bytes, and the id of the key. |
| `versions[].yanked` | Withdrawn: never chosen as the newest, but still installable by asking for exactly that version. |

An index is at most 4 MiB and an archive at most 64 MiB.

**Where an index can be** is the same for a public registry, a mirror and an offline copy:

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
The first index that offers a name decides, in the order of `mining.registries`.

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
memcastle source search claude                  # what the registries offer
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

**Updating** installs the newest version from the index a registry source was installed from.
A source you installed from a file has no upstream and is left alone, and a bundled source is updated with MemCastle:
`update` leaves it out, and `update pi` says so (`memcastle::source::bundled`).
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
A bundled source cannot be removed, since it comes with MemCastle: disable it instead.

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

Releases ship `pi`, `opencode`, `claude`, `codex`, `chatgpt` and `github` unpacked,
one directory each (`<name>/memcastle-source.toml`,
`<name>/source.wasm`), under
`share/memcastle/sources/`:
in the release tarballs, as `/usr/share/memcastle/sources/` in the `.deb`, `.rpm` and AUR package, and under the formula's
`share/memcastle/sources/` with Homebrew.
The release also publishes them alone as `memcastle_<version>_sources.tar.gz`, which is what the AUR and Homebrew recipes
fetch, since they install the raw binary asset and not a tarball.
The daemon finds them beside the binary (the same search as other [runtime assets](configuration.md#runtime-assets)),
or in `mining.bundled_dir`.

They are installed from the start, and run from where the release put them:
`memcastle source list` shows them as `bundled` and `installed`, and `memcastle source enable pi` is all they need.
Enabling is the consent: a bundled source is as trusted as the MemCastle that carries it, so it is not asked to be
agreed to as an installed package is, and `memcastle source show pi` prints what it may read and run.
They are not linked into the binary, and they cannot be installed again, updated or removed;
only whether one is enabled is yours, and it survives an upgrade, which brings the new version of the source with it.
A package file you install under the same name (`memcastle source install ./pi`) is read instead, and removing it brings
the bundled one back.
A standalone binary or a checkout carries none: install the official ones from the registry below.

The set is a line in `packaging/sources/build.sh`, which a release and `mise run sources:package` both run,
and it does not constrain the runtime: any directory under `sources/` could be added.
`directory` is the worked example of the built-in source and is not bundled.

The bundle shares its root with the agent integrations and the skills: `assets.dir` is the one directory a package
installs into, and `sources/` is one of the directories under it (see [Runtime assets](configuration.md#runtime-assets)
and [Agent integrations](integrations.md)).
The `pi`, `opencode` and `claude` sources here mine those agents' session history; they are not integrations that run
inside an agent.

## The official registry

The official registry is a static file in this repository, [`docs/registry.json`](https://github.com/noirbizarre/memcastle/blob/main/docs/registry.json),
published with the documentation at `https://noirbizarre.github.io/memcastle/registry.json`.
It lists no versions: it names the GitHub repositories that publish sources (`pi`, `opencode` and `claude` are published
by this one), and the daemon reads their releases.
Registering a source, or changing where it comes from, is therefore a pull request to that file, merged and deployed like
any documentation change, and publishing a new version is a release of the source's own repository: no new release of
MemCastle and no change to the registry.
It is the default value of `mining.registries`, so on any installation:

```sh
memcastle source search          # lists pi, opencode and claude, and says which are already installed
memcastle source install pi      # on a build with no bundle: after you agree to its permissions
```

Nothing is fetched until you run one of those commands, and a package from it is checked against its SHA-256 and your
`mining.trust` policy like any other registry's.
The official packages are not signed yet, so `trust = "required"` refuses them.
Set `mining.registries` to use other registries instead of it, or to `[]` to use none.

### Registering a GitHub repository

An entry names the repository instead of listing versions:

```json
{
  "name": "claude",
  "description": "Claude Code session history",
  "license": "MIT",
  "repository": "example/memcastle-claude"
}
```

The daemon lists the repository's releases (`GET /repos/example/memcastle-claude/releases`, one request for each
repository however many sources it publishes) and takes from them:

- every release that is neither a draft nor a prerelease;
- the assets named `<name>-<version>.tar.gz`, which is what `memcastle source package` writes, where `<version>` is a
  semantic version and `<name>` is the entry's name;
- for each, the SHA-256 that GitHub computed when the asset was uploaded (the asset's `digest`), which the download is
  then checked against like any other archive.
  An asset with no digest, which GitHub only computes for uploads made since it started, is passed over with a warning:
  upload it again.

The newest release wins when several attach the same version.
What GitHub reports is not a signature, so these packages count as unsigned, and a MemCastle whose `mining.trust` is
`required` refuses them.
The compatibility a package declares is checked when it is downloaded, so `search` may show a version that `install`
then refuses with `memcastle::source::incompatible`.

A repository that cannot be read (it does not exist, or GitHub's unauthenticated limit of 60 requests an hour for an
address is spent) is a warning that names it, its sources are left out, and every other source in the registry still
answers.

Set `GH_TOKEN` or `GITHUB_TOKEN` in the daemon's environment (`GH_TOKEN` wins, as it does for `gh`) to lift that limit.
The token is sent as a bearer token to the releases request and to nothing else: not to the archive downloads, not to a
registry that is not GitHub, and not across a redirect to another host.
A token with no scopes is enough, since the releases of a public repository are public, and a token for a private
repository lets the daemon read its releases.
It is read from the environment only, never from the configuration file, and it is not logged, reported or stored.
`mining.github_api_url` points the daemon at another API, such as a GitHub Enterprise server, and the token is then
presented to that server.

## When something goes wrong

| Code | Meaning |
|---|---|
| `memcastle::source::registry_unavailable` | An index could not be read. The error names the location. |
| `memcastle::source::not_in_registry` | No index offers the name, or none of its versions runs here; the error lists what exists. |
| `memcastle::source::integrity` | The download is not what was published. Nothing was installed. |
| `memcastle::source::untrusted` | The trust policy refuses the package. |
| `memcastle::source::consent_required` | The package asks for permissions you have not agreed to. |

The full list is in [Writing a mining source](writing-sources.md#troubleshooting).
