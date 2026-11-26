#!/usr/bin/env bash
# Replace the .DirIcon symlink of an extracted AppDir with a real PNG copy.
#
# Tauri writes .DirIcon as an absolute symlink into the build machine's
# checkout; on any other machine it dangles and file managers show a generic
# icon.
#   ./fix-diricon.sh SQUASHFS_ROOT
set -euo pipefail

root="${1:?usage: fix-diricon.sh SQUASHFS_ROOT}"
icons="$root/usr/share/icons/hicolor"

src=$(find "$icons" -name histv.png -path '*/512x512/*' -print -quit)
[ -n "$src" ] || src=$(find "$icons" -name histv.png -print -quit)
[ -n "$src" ] || { echo "fix-diricon: no histv.png under $icons" >&2; exit 1; }

rm -f "$root/.DirIcon"
cp "$src" "$root/.DirIcon"
