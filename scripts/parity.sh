#!/usr/bin/env bash
# One-shot parity workflow: fetch test data, build the binary, run the check.
#
# Usage:
#   scripts/parity.sh                 # non-Mirax coverage profile
#   scripts/parity.sh coverage        # one slide per backend
#   scripts/parity.sh --path Aperio/CMU-1-Small-Region.svs   # explicit file(s)
#
# Any arguments are forwarded to download-openslide-testdata.py. The data lands
# in $OPENSLIDE_TESTDATA_DIR (default .tmp/openslide-testdata).
set -euo pipefail

cd "$(dirname "$0")/.."

DATA_DIR="${OPENSLIDE_TESTDATA_DIR:-.tmp/openslide-testdata}"
export OPENSLIDE_TESTDATA_DIR="$DATA_DIR"

# Default selection if the caller passes nothing.
if [ "$#" -eq 0 ]; then
    set -- --profile nonmirax-coverage
fi

echo "==> Downloading test data ($*)"
python3 scripts/download-openslide-testdata.py --extract "$@"

echo "==> Building openslide-pure-rs (release)"
cargo build --release

echo "==> Running parity check"
python3 scripts/parity-check.py --data-dir "$DATA_DIR" --exclude-mirax --json "$DATA_DIR/parity-report.json"
