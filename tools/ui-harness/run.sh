#!/usr/bin/env bash
# Usage: tools/ui-harness/run.sh matrix <ref> | parity | toggle | paint | selftest   (see README.md)
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Never reach for the user's desktop session, whatever the caller's environment says.
unset DISPLAY WAYLAND_DISPLAY
exec python3 "$here/harness.py" "$@"
