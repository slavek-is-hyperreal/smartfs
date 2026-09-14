#!/usr/bin/env bash
#
# fetch-models.sh — download the embedding model weights SmartFS runs on.
#
# Deliberately a separate, explicit step rather than something smartfsd does at
# startup (ADR-63, rejected alternative "auto-download"): a filesystem daemon
# does not reach for the internet while mounting. A missing model is a loud
# error from smartfs-worker, not a reason to fetch 1.2 GB mid-mount.
#
# The repository is public and ungated, so no Hugging Face token is involved.
# If a gated model is ever adopted, the token belongs in the environment
# (HF_TOKEN), never in this file and never in the repo.
#
# Destination, in the same resolution order smartfsd uses for --model-path:
#     $SMARTFS_MODEL_PATH  ->  /var/lib/smartfs/models
#
# On this machine /var is on the system partition with ~5 GB free, so set
# SMARTFS_MODEL_PATH to the pool. Stage 0 asserts the resolved path is not on /.
#
# Usage:  SMARTFS_MODEL_PATH=/vectorlegis_ssd_pool/smartfs-models scripts/fetch-models.sh
# See:    docs/adr/ADR-63-embedding-model-placement.md

set -euo pipefail

MODEL_ROOT="${SMARTFS_MODEL_PATH:-/var/lib/smartfs/models}"
REPO="Qwen/Qwen3-Embedding-0.6B-GGUF"
DEST="${MODEL_ROOT}/qwen3-embedding-0.6b"

# Pinned digests of exactly the artifacts ADR-63 accepted. A weights file that
# does not match is not "a newer upload" to shrug at — it is a different model
# producing a different vector space than the one embedding_models describes.
#
# Both variants are fetched because they serve different execution paths
# (ADR-63 rev 2, sections 1e and 3), not because one is a spare:
#   f16  1137 MiB - CPU path, and GPUs with VRAM to spare
#   Q8_0  604 MiB - GPU path; this is the variant that fits a 1 GB card whole,
#                   with all 28 layers offloaded and ~340 MiB left over
declare -A EXPECTED=(
  [Qwen3-Embedding-0.6B-f16.gguf]=421a27e58d165478cc7acb984a688c2aa41404968b0203e7cd743ece44c54340
  [Qwen3-Embedding-0.6B-Q8_0.gguf]=06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439
)

die() { printf '\033[31m[FAIL]\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[36m[..]\033[0m %s\n' "$*"; }
pass() { printf '\033[32m[OK]\033[0m %s\n' "$*"; }

command -v curl >/dev/null || die "curl is required"
command -v sha256sum >/dev/null || die "sha256sum is required"

case "$MODEL_ROOT" in
  /var/*|/usr/*|/opt/*)
    printf '\033[33m[note]\033[0m %s\n' \
      "MODEL_ROOT=${MODEL_ROOT} is likely on the system partition; ~1.8 GB is needed." ;;
esac

mkdir -p "$DEST" || die "cannot create ${DEST}"

for f in "${!EXPECTED[@]}"; do
  if [[ -f "${DEST}/${f}" ]] \
     && [[ "$(sha256sum "${DEST}/${f}" | cut -d' ' -f1)" == "${EXPECTED[$f]}" ]]; then
    pass "${f} already present and verified"
    continue
  fi
  info "downloading ${f}"
  # -C - resumes a partial file; a truncated 1.2 GB download should not restart.
  curl -fL --retry 3 --retry-delay 3 -C - --progress-bar \
    -o "${DEST}/${f}" "https://huggingface.co/${REPO}/resolve/main/${f}?download=true" \
    || die "download of ${f} failed"

  got="$(sha256sum "${DEST}/${f}" | cut -d' ' -f1)"
  [[ "$got" == "${EXPECTED[$f]}" ]] \
    || die "sha256 mismatch for ${f}
       expected ${EXPECTED[$f]}
       got      ${got}
       Refusing a weights file that is not the one ADR-63 accepted."
  pass "${f} downloaded and verified"
done

# The model card carries the Apache-2.0 licence text; keep it with the weights.
curl -fsL -o "${DEST}/README.md" "https://huggingface.co/${REPO}/resolve/main/README.md" \
  || printf '\033[33m[note]\033[0m could not fetch the model card; weights are fine\n'

( cd "$DEST" && sha256sum ./*.gguf > SHA256SUMS )
pass "models ready in ${DEST}"
du -sh "$DEST"
