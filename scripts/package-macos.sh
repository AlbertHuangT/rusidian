#!/bin/sh
set -eu

repo_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"

iconset=target/packager/Rusidian.iconset
mkdir -p "$iconset" dist

while read -r dimension filename; do
  sips -s format png -z "$dimension" "$dimension" assets/app-icon.svg \
    --out "$iconset/$filename" >/dev/null
done <<'EOF'
16 icon_16x16.png
32 icon_16x16@2x.png
32 icon_32x32.png
64 icon_32x32@2x.png
128 icon_128x128.png
256 icon_128x128@2x.png
256 icon_256x256.png
512 icon_256x256@2x.png
512 icon_512x512.png
1024 icon_512x512@2x.png
EOF

iconutil -c icns "$iconset" -o target/packager/Rusidian.icns
if test -z "${CARGO_PACKAGER_SIGN_PRIVATE_KEY:-}"; then
  unset CARGO_PACKAGER_SIGN_PRIVATE_KEY CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD
fi
"${CARGO_PACKAGER_BIN:-cargo-packager}" --config Packager.toml
