# Parity tooling

Scripts for checking `openslide-pure-rs` against the reference C OpenSlide using
the public [OpenSlide test data](https://openslide.cs.cmu.edu/download/openslide-testdata/).

## Prerequisites

```sh
pip install numpy pillow openslide-python   # reference side + image diffing
# plus the system libopenslide library (e.g. apt install libopenslide0)
```

## Quick start

```sh
scripts/parity.sh                 # download the smoke profile, build, and check
```

This downloads a small set of CC0 slides, builds the release binary, and runs
the parity check, writing a JSON report next to the data.

## Scripts

### `download-openslide-testdata.py`

Downloads selected test data with SHA-256 verification into
`$OPENSLIDE_TESTDATA_DIR` (default `.tmp/openslide-testdata`, git-ignored).

```sh
python3 scripts/download-openslide-testdata.py --list
python3 scripts/download-openslide-testdata.py --profile smoke --extract
python3 scripts/download-openslide-testdata.py --format aperio --extract
python3 scripts/download-openslide-testdata.py --path Mirax/CMU-1-Saved-1_16.zip --extract
```

By default only CC0-licensed files are allowed; pass `--allow-distributable`
for the rest. `.zip` archives (Mirax, DICOM, …) need `--extract`.

### `parity-check.py`

Compares each discovered slide against the reference:

* **metadata** — vendor, level count, per-level dimensions/downsamples, key
  `openslide.*` properties (mpp, objective-power, bounds), associated images.
* **pixels** — for brightfield (3-channel RGB) slides, renders sample regions
  with both implementations and reports max / mean absolute per-channel
  difference and exact-match fraction. Comparison is masked to fully-opaque
  reference pixels (OpenSlide returns premultiplied alpha).

```sh
python3 scripts/parity-check.py                       # discover under data dir
python3 scripts/parity-check.py path/to/slide.svs     # specific slide(s)
python3 scripts/parity-check.py --no-pixels           # metadata only
python3 scripts/parity-check.py --fail-on-pixels --pixel-tol 1.0
python3 scripts/parity-check.py --json report.json
```

The Rust side is driven through the binary's `meta` (JSON metadata) and `read`
(region → PNG) subcommands. Exit status is non-zero when a *hard* metadata
check fails (vendor, level count, dimensions); pixel differences are reported
but only fail the run with `--fail-on-pixels`.
