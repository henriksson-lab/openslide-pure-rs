# Documentation Index

This directory keeps policy, operational, and API notes that are too detailed
for the top-level README.

## User-Facing APIs

- [Lossy compressed tile extraction](compressed-extraction.md): public API,
  supported byte modes, unsupported cases, and OME-Zarr caveats.

## Audit And Maturity

- [Reader status policy](status-policy.md): maturity labels and promotion
  gates.
- [Reader maturity report](maturity-report.md): generated reader status,
  matrix coverage, blockers, and runner state.
- [Codec and unsupported-layout policy](codec-policy.md): current codec support
  and explicit rejection rules.
- [Memory and error policy](memory-error-policy.md): bounded memory, cache, RSS,
  and error behavior requirements.
- [Fixture sourcing](fixture-sourcing.md): finding missing local/public
  fixtures.

## Benchmark Operations

- [Stable benchmark runner](benchmark-runner.md): stable-runner contract,
  registration, maintenance, baseline refresh, and failure triage.
