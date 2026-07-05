#!/usr/bin/env bash
set -euo pipefail

profile="${OPENSLIDE_AUDIT_RUNNER_PROFILE:-openslide-audit-stable-v1}"
jobs="${OPENSLIDE_AUDIT_JOBS:-1}"
data_dir="${OPENSLIDE_TESTDATA_DIR:-.tmp/openslide-testdata}"
report_dir="${OPENSLIDE_AUDIT_REPORT_DIR:-${data_dir}}"
region_size="${OPENSLIDE_AUDIT_REGION_SIZE:-128}"
regions_per_level="${OPENSLIDE_AUDIT_REGIONS_PER_LEVEL:-1}"

mkdir -p "${report_dir}"

cargo build --release --examples

python3 scripts/bench-realdata.py \
  --jobs "${jobs}" \
  --runner-profile "${profile}" \
  --region-size "${region_size}" \
  --regions-per-level "${regions_per_level}" \
  --json "${report_dir}/bench-stable.json"

python3 scripts/check-audit-baselines.py \
  --bench-report "${report_dir}/bench-stable.json"
