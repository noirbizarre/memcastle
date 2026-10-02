#!/usr/bin/env bash
# Build the .deb and .rpm for every Linux binary staged in a directory.
#
# Usage: packaging/nfpm/build.sh <version> <dist-dir>
#
# Reads  <dist-dir>/memcastle_<version>_linux-{amd64,arm64}
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
