#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

report_dir="${OPENSLIDE_AUDIT_REPORT_DIR:-.tmp/openslide-testdata}"
mkdir -p "${report_dir}"

cargo fmt --check
python3 scripts/check-audit-baselines.py
python3 scripts/check-runner-status-update.py
python3 scripts/check-mature-runner-gate.py

python3 scripts/find-fixture-candidates.py \
  --missing-from-reader-status fixtures/reader-status.toml \
  --json "${report_dir}/fixture-candidates.json"

python3 scripts/check-audit-baselines.py \
  --fixture-candidates-report "${report_dir}/fixture-candidates.json" \
  --fail-on-fixture-candidates

if [ "${OPENSLIDE_AUDIT_FULL:-0}" = "1" ]; then
  cargo check --all-targets
  cargo test
  cargo clippy --all-targets -- -D warnings
  cargo build --release --examples
fi

if [ "${OPENSLIDE_AUDIT_PACKAGE:-0}" = "1" ]; then
  cargo package --no-verify

  crate="$(find target/package -maxdepth 1 -type f -name 'openslide-pure-rs-*.crate' | sort -V | tail -n 1)"
  if [ -z "${crate}" ]; then
    echo "no packaged crate found under target/package" >&2
    exit 1
  fi

  rm -rf .tmp/package-source .tmp/package-install
  mkdir -p .tmp/package-source
  tar -xzf "${crate}" -C .tmp/package-source

  package_dir="$(find .tmp/package-source -maxdepth 1 -type d -name 'openslide-pure-rs-*' | sort -V | tail -n 1)"
  if [ -z "${package_dir}" ]; then
    echo "no packaged source directory found under .tmp/package-source" >&2
    exit 1
  fi

  cargo install --path "${package_dir}" --root .tmp/package-install --debug --locked --offline
  test -x .tmp/package-install/bin/openslide-pure-rs
fi
