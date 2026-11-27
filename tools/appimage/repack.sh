#!/usr/bin/env bash
# Extract an AppImage, repair its .DirIcon and repack it.
#   APPIMAGETOOL=./appimagetool ./repack.sh IN.AppImage OUT.AppImage
# IN and OUT may be the same file. The appimagetool binary is fetched and
# checksum-verified by the workflow before this runs.
set -euo pipefail

in="${1:?usage: repack.sh IN.AppImage OUT.AppImage}"
out="${2:?usage: repack.sh IN.AppImage OUT.AppImage}"
tool="${APPIMAGETOOL:-./appimagetool}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

in=$(realpath "$in")
out=$(realpath -m "$out")
tool=$(realpath "$tool")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cp "$in" "$work/in.AppImage"
chmod +x "$work/in.AppImage"
(cd "$work" && ./in.AppImage --appimage-extract >/dev/null)

"$here/fix-diricon.sh" "$work/squashfs-root"

# The extracted modes depend on the packager's umask, and files added above
# are created under it. Make everything readable (and directories and
# executables traversable) by all, so the image does not depend on it.
chmod -R a+rX "$work/squashfs-root"

ARCH=x86_64 "$tool" --appimage-extract-and-run "$work/squashfs-root" "$work/out.AppImage" >/dev/null
mv -f "$work/out.AppImage" "$out"
