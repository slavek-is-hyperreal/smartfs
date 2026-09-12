#!/usr/bin/env bash
#
# build.sh — the versioned build entrypoint for the SmartFS workspace.
#
# Bumps the patch version of every crate whose compilable content changed since
# the last recorded build, then runs cargo. Use this instead of a bare
# `cargo build`: a bare cargo invocation compiles happily without touching any
# version number, which is exactly what this is here to prevent.
#
# Usage:
#   scripts/build.sh                             # cargo build --workspace
#   scripts/build.sh clippy --all-targets -- -D warnings
#   scripts/build.sh test --workspace
#   scripts/build.sh --no-bump build --workspace # skip the bump (CI verification)
#
# See scripts/version_bump.py for how a crate's content hash is computed.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

BUMP=1
if [[ "${1:-}" == "--no-bump" ]]; then
  BUMP=0
  shift
fi

# Default to the whole workspace when no cargo subcommand is given.
CARGO_SUBCOMMANDS="build check test clippy run doc bench fix"
if [[ $# -eq 0 ]]; then
  set -- build --workspace
elif [[ ! " $CARGO_SUBCOMMANDS " == *" $1 "* ]]; then
  set -- build --workspace "$@"
fi

if (( BUMP )); then
  python3 "${REPO}/scripts/version_bump.py"
else
  echo "build.sh: --no-bump requested; versions left untouched"
fi

echo "build.sh: cargo $*"
exec cargo "$@"
