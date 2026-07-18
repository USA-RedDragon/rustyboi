#!/usr/bin/env bash
# Single source of truth for the PGO actions-cache key. Computed in shell (not
# workflow hashFiles) so the save side (pgo.yaml) and the poll/restore side
# (.github/actions/pgo-restore) can never drift, and consumers can poll the key
# through the caches API. The v2 prefix retires the old hashFiles-keyed caches.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "pgo-v2-$(sha256sum Cargo.lock tools/rust-cross.sh | sha256sum | cut -c1-16)"
