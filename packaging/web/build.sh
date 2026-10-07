#!/usr/bin/env bash
# Build the web UI that ships with MemCastle, and optionally lay it out as a package tree.
#
# Usage: packaging/web/build.sh [out-dir]
#
# Always: builds `web/` into `web/dist/`, the static files the daemon serves under `/ui` when `web.enable` is set. That
# is also all the development mode needs: `memcastle serve --assets-dir <checkout>` finds `web/dist/index.html` there,
# at the same relative path a package installs it to (docs/adr/035).
#
# With <out-dir>: also writes the tree an OS package installs under `share/memcastle/` -
#   <out-dir>/web/dist/...
# and checks that nothing which must not ship came along. One script for 📦 Publish Release and for
# `mise run web:package`, so what a release bundles is what a developer can build and test.
#
# Needs bun (installs and runs the scripts) and node (which vite and vue-tsc run on).
set -euo pipefail

if [ "$#" -gt 1 ]; then
  echo "usage: $0 [out-dir]" >&2
  exit 2
fi
out="${1:-}"

cd "$(dirname "$0")/../.."
root="$PWD"

# bun and node may be provided by mise (see `mise.toml`), which `mise run` puts on the PATH; a bare shell needs them.
for tool in bun node; do
  if ! command -v "${tool}" > /dev/null; then
    echo "error: ${tool} is not on the PATH; install it, or run this through 'mise run web:build'" >&2
    exit 1
  fi
done

# A stale build would be served if the build below failed half-way and left it.
rm -rf "${root}/web/dist"
(
  cd "${root}/web"
  # --frozen-lockfile: a lockfile that disagrees with package.json is a failure, not a silent rewrite.
  bun install --frozen-lockfile > /dev/null
  bun run build > /dev/null
)
test -f "${root}/web/dist/index.html" || { echo "error: building the dashboard produced no index.html" >&2; exit 1; }

if [ -z "${out}" ]; then
  exit 0
fi

mkdir -p "${out}"
out="$(realpath "${out}")"
# A stale tree from an earlier run would ship files that no longer exist in the sources.
rm -rf "${out}/web"
mkdir -p "${out}/web"
cp -r "${root}/web/dist" "${out}/web/dist"

# What must never ship: the sources, their dependencies, and source maps, which would let the daemon serve the sources.
unwanted=(-name node_modules -o -name '*.ts' -o -name '*.vue' -o -name '*.map' -o -name bun.lock -o -name package.json)
if find "${out}" \( "${unwanted[@]}" \) -print -quit | grep -q .; then
  echo "error: ${out} holds sources or dependencies that must not ship:" >&2
  find "${out}" \( "${unwanted[@]}" \) >&2
  exit 1
fi
test -f "${out}/web/dist/index.html" || { echo "error: ${out} has no web/dist/index.html" >&2; exit 1; }
