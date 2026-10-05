#!/usr/bin/env bash
# Package the sources that ship with MemCastle into one directory, with the registry index that lists them.
#
# Usage: packaging/sources/build.sh <memcastle-binary> <out-dir>
#
# Writes <out-dir>/<name>-<version>.tar.gz for each bundled source and <out-dir>/memcastle-index.json.
#
# Bundled sources are ordinary packages (docs/adr/033), not code linked into the binary: this is the same
# `memcastle source package` and `memcastle source index` a third-party author runs, and the result is what
# `memcastle source install <name>` finds with no registry and no network once it sits under
# `share/memcastle/sources/`. One script for 📦 Publish Release and for `mise run sources:package`, so what a release
# bundles is what a developer can build and test.
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
# A stale package from an earlier run would be indexed beside the new one.
rm -f "${out}"/*.tar.gz "${out}"/*.tar.gz.sha256 "${out}/memcastle-index.json"

for source in "${BUNDLED[@]}"; do
  # Packaged where the project's own `dist/` is, so a leftover package of an older version cannot be picked up.
  # Only the archives are removed, never `dist/` itself: the `wasm_*` tests build the component into that directory
  # from other processes while this runs, and deleting `source.wasm` under them fails them with "No such file".
  rm -f "sources/${source}"/dist/*.tar.gz "sources/${source}"/dist/*.tar.gz.sha256
  "${binary}" source package "sources/${source}" > /dev/null
  cp "sources/${source}"/dist/*.tar.gz "sources/${source}"/dist/*.tar.gz.sha256 "${out}/"
done

"${binary}" source index --name "MemCastle bundled sources" --output "${out}/memcastle-index.json" "${out}"/*.tar.gz
