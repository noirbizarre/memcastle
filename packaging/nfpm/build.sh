#!/usr/bin/env bash
# Build the .deb and .rpm for every Linux binary staged in a directory.
#
# Usage: packaging/nfpm/build.sh <version> <dist-dir>
#
# Reads  <dist-dir>/memcastle_<version>_linux-{amd64,arm64}
# Runs   the amd64 binary once, to print its shell completion scripts
# Writes <dist-dir>/memcastle_<version>_linux-{amd64,arm64}.{deb,rpm}
#
# One script for 📦 Publish Release and for CI's packaging check, so the check
# exercises exactly the code the release runs. The first release failed here
# (a `${BINARY}` that nfpm did not expand) because nothing ran this before a tag.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <version> <dist-dir>" >&2
  exit 2
fi
version="$1"
dist="$2"

# Deterministic package timestamps; callers may set it, the commit time is the default.
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
export SOURCE_DATE_EPOCH

# Completion scripts come from running the binary, so they always match its
# commands and flags. They are plain text and identical on every architecture,
# so the amd64 binary (the only one a CI runner can execute) serves both
# packages. The staged file lost its execute bit in the artifact download.
completions="$(mktemp -d)"
trap 'rm -rf "${completions}"' EXIT
runner="${completions}/memcastle"
amd64="${dist}/memcastle_${version}_linux-amd64"
if [ ! -f "${amd64}" ]; then
  echo "error: ${amd64} is missing; stage the binaries before packaging" >&2
  exit 1
fi
cp "${amd64}" "${runner}"
chmod +x "${runner}"
for shell in bash zsh fish; do
  "${runner}" completions "${shell}" > "${completions}/memcastle.${shell}"
  # A binary that prints nothing would ship a package whose completion file is
  # empty, and nothing else would notice.
  if [ ! -s "${completions}/memcastle.${shell}" ]; then
    echo "error: \`memcastle completions ${shell}\` printed nothing" >&2
    exit 1
  fi
done
export COMPLETIONS_DIR="${completions}"

# The sources that ship with MemCastle (docs/adr/033): packages and their index, built by packaging/sources/build.sh.
# An empty directory when the caller has none, so a package built without them is a package without them and not an
# nfpm error about a path that does not exist.
export SOURCES_DIR="${SOURCES_DIR:-${completions}/sources}"
mkdir -p "${SOURCES_DIR}"

for asset in linux-amd64 linux-arm64; do
  binary="${dist}/memcastle_${version}_${asset}"
  # A missing binary would otherwise surface as nfpm's glob error, which names
  # the unexpanded template and not the file that is actually absent.
  if [ ! -f "${binary}" ]; then
    echo "error: ${binary} is missing; stage the binaries before packaging" >&2
    exit 1
  fi
  export VERSION="${version}"
  export BINARY="${binary}"
  for format in deb rpm; do
    # deb and rpm name the same architectures differently.
    case "${format}-${asset}" in
      deb-linux-amd64) NFPM_ARCH=amd64 ;;
      deb-linux-arm64) NFPM_ARCH=arm64 ;;
      rpm-linux-amd64) NFPM_ARCH=x86_64 ;;
      rpm-linux-arm64) NFPM_ARCH=aarch64 ;;
    esac
    export NFPM_ARCH
    nfpm package --config packaging/nfpm/nfpm.yaml --packager "${format}" \
      --target "${dist}/memcastle_${version}_${asset}.${format}"
  done
done
