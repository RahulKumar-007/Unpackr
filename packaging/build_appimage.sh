#!/usr/bin/env bash
set -euo pipefail

VERSION="${VERSION:-0.1.0}"
ARCH="${ARCH:-x86_64}"
DIST_DIR="${DIST_DIR:-$(pwd)/dist}"
BIN_SOURCE="${BIN_SOURCE:-target/release/unpackr}"
APPDIR="/tmp/unpackr.AppDir"
OUT_APPIMAGE="${DIST_DIR}/unpackr-v${VERSION}-${ARCH}.AppImage"

if [ ! -f "${BIN_SOURCE}" ]; then
    echo "Compiling release binary..."
    cargo build --release
fi

echo "Staging AppDir in ${APPDIR}..."
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/scalable/apps"
mkdir -p "${APPDIR}/usr/share/metainfo"

# 1. Install binary
install -m 755 "${BIN_SOURCE}" "${APPDIR}/usr/bin/unpackr"
strip -s "${APPDIR}/usr/bin/unpackr" || true

# 2. Desktop file and Icons
install -m 644 extra/unpackr.desktop "${APPDIR}/usr/share/applications/unpackr.desktop"
install -m 644 extra/unpackr.desktop "${APPDIR}/unpackr.desktop"

install -m 644 extra/unpackr.svg "${APPDIR}/usr/share/icons/hicolor/scalable/apps/unpackr.svg"
install -m 644 extra/unpackr.svg "${APPDIR}/unpackr.svg"
install -m 644 extra/unpackr.svg "${APPDIR}/.DirIcon"

if [ -f packaging/flatpak/io.github.rahulkumar_007.Unpackr.metainfo.xml ]; then
    install -m 644 packaging/flatpak/io.github.rahulkumar_007.Unpackr.metainfo.xml \
        "${APPDIR}/usr/share/metainfo/io.github.rahulkumar_007.Unpackr.metainfo.xml"
    install -m 644 packaging/flatpak/io.github.rahulkumar_007.Unpackr.metainfo.xml \
        "${APPDIR}/usr/share/metainfo/unpackr.appdata.xml"
fi

# 3. Create AppRun launcher
cat << 'EOF' > "${APPDIR}/AppRun"
#!/bin/sh
SELF=$(readlink -f "$0")
HERE=${SELF%/*}
export PATH="${HERE}/usr/bin:${PATH}"
exec "${HERE}/usr/bin/unpackr" "$@"
EOF
chmod +x "${APPDIR}/AppRun"

# 4. Locate or download appimagetool
APPIMAGETOOL="${APPIMAGETOOL:-/tmp/appimagetool}"
if [ ! -x "${APPIMAGETOOL}" ]; then
    echo "Downloading appimagetool..."
    curl -sLo "${APPIMAGETOOL}" "https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage"
    chmod +x "${APPIMAGETOOL}"
fi

mkdir -p "${DIST_DIR}"
echo "Building AppImage..."
# Use --appimage-extract-and-run for compatibility in environments without direct FUSE mount access
export ARCH="${ARCH}"
if "${APPIMAGETOOL}" --version >/dev/null 2>&1; then
    "${APPIMAGETOOL}" -n "${APPDIR}" "${OUT_APPIMAGE}"
else
    "${APPIMAGETOOL}" --appimage-extract-and-run -n "${APPDIR}" "${OUT_APPIMAGE}"
fi

echo "AppImage created successfully: ${OUT_APPIMAGE}"
ls -lh "${OUT_APPIMAGE}"
chmod +x "${OUT_APPIMAGE}"
