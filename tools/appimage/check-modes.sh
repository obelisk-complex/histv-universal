#!/usr/bin/env bash
# Check that the modes stored in an AppImage do not depend on the packager's
# umask: every file and directory is world-readable, and every directory and
# every file the owner can execute is world-executable. Needs unsquashfs.
# (--appimage-extract is no use here: it re-creates directories as 0700.)
#   ./check-modes.sh FILE.AppImage
set -euo pipefail

img="${1:?usage: check-modes.sh FILE.AppImage}"
img=$(realpath "$img")
command -v unsquashfs >/dev/null || { echo "check-modes: unsquashfs not found" >&2; exit 2; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cp "$img" "$work/app.AppImage"
chmod +x "$work/app.AppImage"
offset=$("$work/app.AppImage" --appimage-offset)

# A listing line is "drwxr-xr-x root/root SIZE DATE TIME PATH". Symlinks carry
# no mode of their own, so they are skipped.
bad=$(unsquashfs -o "$offset" -ll "$work/app.AppImage" | awk '
  /^[-d][-rwxsStT]{9} / {
    m = $1
    ok = substr(m, 8, 1) == "r"
    if (substr(m, 1, 1) == "d" || substr(m, 4, 1) ~ /[xs]/)
      ok = ok && substr(m, 10, 1) ~ /[xt]/
    if (!ok) { path = $0; sub(/^[^ ]+ +[^ ]+ +[0-9]+ +[^ ]+ +[^ ]+ +/, "", path); print m, path }
  }')

if [ -n "$bad" ]; then
  echo "FAIL these paths are not readable (and executable where needed) by everyone:"
  echo "$bad" | head -20
  echo "($(echo "$bad" | wc -l) in total)"
  exit 1
fi
echo "ok   all files and directories are world-readable"
