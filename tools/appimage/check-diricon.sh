#!/usr/bin/env bash
# Check that an AppImage's .DirIcon is a regular PNG file and that the Linux
# Mint thumbnailer can render the AppImage.
#   ./check-diricon.sh FILE.AppImage
set -euo pipefail

img="${1:?usage: check-diricon.sh FILE.AppImage}"
img=$(realpath "$img")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cp "$img" "$work/app.AppImage"
chmod +x "$work/app.AppImage"
(cd "$work" && ./app.AppImage --appimage-extract >/dev/null)

icon="$work/squashfs-root/.DirIcon"
fail=0
if [ -L "$icon" ]; then
  echo "FAIL .DirIcon is a symlink -> $(readlink "$icon")"; fail=1
elif [ ! -f "$icon" ]; then
  echo "FAIL .DirIcon is not a regular file"; fail=1
elif [ "$(head -c 8 "$icon" | od -An -tx1 | tr -d ' \n')" != "89504e470d0a1a0a" ]; then
  echo "FAIL .DirIcon is not a PNG"; fail=1
else
  echo "ok   .DirIcon is a regular PNG file"
fi

if log=$(xapp-appimage-thumbnailer -i "$work/app.AppImage" -o "$work/thumb.png" -s 128 2>&1) \
  && [ -s "$work/thumb.png" ] \
  && [ "$(head -c 8 "$work/thumb.png" | od -An -tx1 | tr -d ' \n')" = "89504e470d0a1a0a" ]; then
  echo "ok   xapp-appimage-thumbnailer rendered a PNG"
else
  echo "FAIL xapp-appimage-thumbnailer: $log"; fail=1
fi

exit "$fail"
