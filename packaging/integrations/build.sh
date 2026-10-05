#!/usr/bin/env bash
# Build the agent integrations that ship with MemCastle, and optionally lay them out as a package tree.
#
# Usage: packaging/integrations/build.sh [out-dir]
#
# Always: bundles each integration into `integrations/<id>/dist/`, one self-contained JavaScript file (plus, for Pi,
# the `package.json` Pi reads) with every dependency inlined. That is what makes the official installation path need no
# npm publication and no `bun install` on the user's machine (docs/adr/034), and it is also all the development mode
# needs: `memcastle integration install <id> --assets-dir <checkout>` installs from these `dist/` directories.
#
# With <out-dir>: also writes the tree an OS package installs under `share/memcastle/` -
#   <out-dir>/integrations/<id>/{memcastle-integration.toml,dist/}
#   <out-dir>/skills/<name>/...
# and checks that nothing which must not ship came along. One script for 📦 Publish Release and for
# `mise run integrations:package`, so what a release bundles is what a developer can build and test.
#
# Needs bun. The agents' own SDKs stay external: the agent provides them (they are type-only imports, plus OpenCode's
# optional `tool` helper), and a second copy in the bundle would be a second copy of its classes.
set -euo pipefail

if [ "$#" -gt 1 ]; then
  echo "usage: $0 [out-dir]" >&2
  exit 2
fi
out="${1:-}"

# Which integrations ship is a packaging decision and does not constrain the runtime (docs/adr/034): any directory
# under `integrations/` with a `memcastle-integration.toml` and a `dist/` could be added here.
BUNDLED=(pi opencode)

cd "$(dirname "$0")/../.."
root="$PWD"

# bun may be provided by mise (see `mise.toml`), which `mise run` puts on the PATH; a bare shell needs it installed.
if ! command -v bun > /dev/null; then
  echo "error: bun is not on the PATH; install it (https://bun.sh), or run this through 'mise run integrations:build'" >&2
  exit 1
fi

bundle() {
  local id="$1" entry="$2"
  shift 2
  local dir="${root}/integrations/${id}"
  # A stale bundle would be installed if the build below failed half-way and left it.
  rm -rf "${dir}/dist"
  (
    cd "${dir}"
    # --frozen-lockfile: a lockfile that disagrees with package.json is a failure, not a silent rewrite.
    bun install --frozen-lockfile > /dev/null
    # `--target=node` because nothing here uses a Bun API, so the bundle loads under any runtime an agent embeds.
    bun build "${entry}" --outdir dist --target=node "$@" > /dev/null
  )
  test -f "${dir}/dist/$(basename "${entry%.ts}").js" || { echo "error: bundling ${id} produced no output" >&2; exit 1; }
}

bundle pi src/extension.ts --external '@earendil-works/*'
# Pi reads its entry points from the `package.json` of the package it installs, which here is the installed copy.
# No `dependencies`: the bundle has everything, and Pi does not install a local package's dependencies anyway.
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' integrations/pi/memcastle-integration.toml | head -n 1)"
cat > integrations/pi/dist/package.json << EOF
{
  "name": "memcastle-pi",
  "version": "${version}",
  "private": true,
  "description": "MemCastle lifecycle extension for Pi, installed by 'memcastle integration install pi'",
  "type": "module",
  "license": "MIT",
  "pi": {
    "extensions": ["./extension.js"]
  }
}
EOF

# OpenCode 1 treats every export of a plugin module as a plugin; the entry exports only `default`, and bun keeps it so.
bundle opencode src/index.ts --external '@opencode-ai/*' --external '@opencode/*'

if [ -z "${out}" ]; then
  exit 0
fi

mkdir -p "${out}"
out="$(realpath "${out}")"
# A stale tree from an earlier run would ship files that no longer exist in the sources.
rm -rf "${out}/integrations" "${out}/skills"
mkdir -p "${out}/integrations" "${out}/skills"

for id in "${BUNDLED[@]}"; do
  mkdir -p "${out}/integrations/${id}"
  cp "integrations/${id}/memcastle-integration.toml" "${out}/integrations/${id}/"
  cp -r "integrations/${id}/dist" "${out}/integrations/${id}/dist"
done

# Only the skills themselves: `skills/README.md` is a working document of the repository.
for skill in skills/*/; do
  cp -r "${skill%/}" "${out}/skills/"
done

# What must never ship: the sources the bundle replaced, and their dependencies.
if find "${out}" \( -name node_modules -o -name '*.ts' -o -name bun.lock \) -print -quit | grep -q .; then
  echo "error: ${out} holds sources or dependencies that must not ship:" >&2
  find "${out}" \( -name node_modules -o -name '*.ts' -o -name bun.lock \) >&2
  exit 1
fi
