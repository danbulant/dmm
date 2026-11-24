#!/usr/bin/env bash
set -euo pipefail

binary=${1:?usage: bash packaging/appimage/build.sh BINARY OUTPUT}
output=${2:?usage: bash packaging/appimage/build.sh BINARY OUTPUT}
assets=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

fetch() {
    local file=$work/$1 url=$2 checksum=$3
    curl --fail --location --silent --show-error --retry 3 -o "$file" "$url"
    printf '%s  %s\n' "$checksum" "$file" | sha256sum --check --status
}

fetch linuxdeploy.AppImage \
    https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage \
    c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
fetch appimagetool.AppImage \
    https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
    ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
fetch runtime \
    https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
    2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
chmod +x "$work"/*.AppImage

export APPIMAGE_EXTRACT_AND_RUN=1
"$work/linuxdeploy.AppImage" \
    --appdir "$work/DMM.AppDir" \
    --executable "$binary" \
    --desktop-file "$assets/com.dmm.ModManager.desktop" \
    --icon-file "$assets/com.dmm.ModManager.svg"
install -Dm644 "$assets/../licenses/rars.txt" "$work/DMM.AppDir/usr/share/licenses/dmm/rars.txt"
ARCH=x86_64 "$work/appimagetool.AppImage" --no-appstream \
    --runtime-file "$work/runtime" "$work/DMM.AppDir" "$output"
