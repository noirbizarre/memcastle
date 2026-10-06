#!/usr/bin/env bash
# Package the sources that ship with MemCastle: the bundle a release carries, and the registry that lists them.
#
# Usage: packaging/sources/build.sh <memcastle-binary> <out-dir>
#
# Writes, under <out-dir>:
#   sources/<name>/            each source unpacked (memcastle-source.toml, source.wasm, ...): the bundle, which goes to
#                              `share/memcastle/sources/` and is installed from the start, run in place (docs/adr/039)
#   archives/<name>-<ver>.tar.gz and .sha256
#                              the same packages as archives: what the registry serves, attached to the release
#   memcastle-index.json       the registry index listing the archives, published with the documentation site
#
# Environment:
#   SOURCES_BASE_URL       where the archives will be served (the release's download URL); without it the index lists
#                          bare file names, which resolve beside the index (what a local registry directory is)
#   SOURCES_PREVIOUS_INDEX the index published so far; this one extends it, so a registry keeps every version it ever
#                          offered. A version already listed is not indexed again: a rebuilt archive is not
#                          byte-identical, and the published digest must keep meaning what it was published as.
#
# Bundled sources are ordinary packages (docs/adr/033), not code linked into the binary: this is the same
# `memcastle source package` and `memcastle source index` a third-party author runs. One script for 📦 Publish Release
# and for `mise run sources:package`, so what a release bundles is what a developer can build and test.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <memcastle-binary> <out-dir>" >&2
  exit 2
fi
binary="$(realpath "$1")"
out="$2"

# Which sources ship is a packaging decision and does not constrain the runtime (docs/adr/033): any directory under
# `sources/` could be added here. `directory` is the worked example of the built-in source and is not bundled.
BUNDLED=(pi opencode)

cd "$(dirname "$0")/../.."
mkdir -p "${out}"
out="$(realpath "${out}")"
# A stale package from an earlier run would be bundled and indexed beside the new one.
rm -rf "${out}/sources" "${out}/archives" "${out}/memcastle-index.json"
mkdir -p "${out}/sources" "${out}/archives"

previous="${SOURCES_PREVIOUS_INDEX:-}"
if [ -n "${previous}" ] && [ -f "${previous}" ]; then
  cp "${previous}" "${out}/memcastle-index.json"
else
  previous=""
fi

indexable=()
for source in "${BUNDLED[@]}"; do
  # Packaged where the project's own `dist/` is, so a leftover package of an older version cannot be picked up.
  # Only the archives are removed, never `dist/` itself: the `wasm_*` tests build the component into that directory
  # from other processes while this runs, and deleting `source.wasm` under them fails them with "No such file".
  rm -f "sources/${source}"/dist/*.tar.gz "sources/${source}"/dist/*.tar.gz.sha256
  "${binary}" source package "sources/${source}" > /dev/null

  archive="$(ls "sources/${source}"/dist/*.tar.gz)"
  file="$(basename "${archive}")"

  # The bundle: the archive's own files, so what runs is what the registry serves.
  mkdir -p "${out}/sources/${source}"
  tar -xzf "${archive}" -C "${out}/sources/${source}"

  # The registry: only a version it does not already list.
  if [ -n "${previous}" ] && grep -qF "${file}\"" "${previous}"; then
    echo "${file} is already in the published index; keeping its archive" >&2
    continue
  fi
  cp "${archive}" "${archive}.sha256" "${out}/archives/"
  indexable+=("${out}/archives/${file}")
done

# With nothing new, the copied previous index already is the index; `source index` extends the file at --output.
if [ "${#indexable[@]}" -gt 0 ]; then
  args=(--name "MemCastle official sources" --output "${out}/memcastle-index.json")
  if [ -n "${SOURCES_BASE_URL:-}" ]; then
    args+=(--base-url "${SOURCES_BASE_URL}")
  fi
  "${binary}" source index "${args[@]}" "${indexable[@]}"
fi
