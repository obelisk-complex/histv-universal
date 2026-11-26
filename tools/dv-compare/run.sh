#!/usr/bin/env bash
# dv-compare: build the RPU helper if needed, then run the tool.
#   ./run.sh compare INPUT OUTPUT [--mode copy|reencode] [options]
#   ./run.sh selftest
# Env: FFMPEG / FFPROBE (default: from PATH), DVC_TARGET (cargo target dir, default rpu-dump/target).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="${DVC_TARGET:-$here/rpu-dump/target}"
bin="$target/release/dvc-rpu"

if [[ ! -x "$bin" || -n "$(find "$here/rpu-dump/src" "$here/rpu-dump/Cargo.toml" -newer "$bin" -print -quit)" ]]; then
  CARGO_TARGET_DIR="$target" cargo build --release --locked --quiet --manifest-path "$here/rpu-dump/Cargo.toml" >&2
fi

export DVC_RPU="${DVC_RPU:-$bin}"
PYTHONPATH="$here" PYTHONDONTWRITEBYTECODE=1 exec python3 -m dvcompare "$@"
