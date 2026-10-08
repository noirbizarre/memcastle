#!/usr/bin/env bash
# Package the sources that ship with MemCastle: the bundle a release carries, and the archives it attaches.
#
# Usage: packaging/sources/build.sh <memcastle-binary> <out-dir>
#
# Writes, under <out-dir>:
#   sources/<name>/            each source unpacked (memcastle-source.toml, source.wasm, ...): the bundle, which goes to
#                              `share/memcastle/sources/` and is installed from the start, run in place (docs/adr/040)
#   archives/<name>-<ver>.tar.gz and .sha256
#                              the same packages as archives, attached to the release: what the official registry
#                              resolves to when a daemon with no bundle installs `pi` or `opencode`
#
# There is no registry index here. The official registry is a static file in the documentation
# (`docs/registry.json`) that names this repository, and a daemon reads its releases, so publishing a
# source is attaching `<name>-<version>.tar.gz` to a release, and registering one is a pull request (docs/adr/040).
#
# Bundled sources are ordinary packages (docs/adr/033), not code linked into the binary: this is the same
# `memcastle source package` a third-party author runs. One script for 📦 Publish Release and for
# `mise run sources:package`, so what a release bundles is what a developer can build and test.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <memcastle-binary> <out-dir>" >&2
  exit 2
fi
binary="$(realpath "$1")"
out="$2"

# Which sources ship is a packaging decision and does not constrain the runtime (docs/adr/033): any directory under
# `sources/` could be added here. `directory` is the worked example of the built-in source and is not bundled.
BUNDLED=(pi opencode claude codex chatgpt)

cd "$(dirname "$0")/../.."
mkdir -p "${out}"
out="$(realpath "${out}")"
# A stale package from an earlier run would be bundled beside the new one.
rm -rf "${out}/sources" "${out}/archives"
mkdir -p "${out}/sources" "${out}/archives"

for source in "${BUNDLED[@]}"; do
  # Packaged where the project's own `dist/` is, so a leftover package of an older version cannot be picked up.
  # Only the archives are removed, never `dist/` itself: the `wasm_*` tests build the component into that directory
  # from other processes while this runs, and deleting `source.wasm` under them fails them with "No such file".
  rm -f "sources/${source}"/dist/*.tar.gz "sources/${source}"/dist/*.tar.gz.sha256
  "${binary}" source package "sources/${source}" > /dev/null

  archive="$(ls "sources/${source}"/dist/*.tar.gz)"

  # The bundle: the archive's own files, so what runs is what the registry serves.
  mkdir -p "${out}/sources/${source}"
  tar -xzf "${archive}" -C "${out}/sources/${source}"

  # The release asset, under the name the registry looks for: `<name>-<version>.tar.gz`.
  cp "${archive}" "${archive}.sha256" "${out}/archives/"
done
