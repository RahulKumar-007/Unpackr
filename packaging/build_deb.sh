#!/usr/bin/env bash
set -euo pipefail

VERSION="${VERSION:-0.1.0}"
ARCH="${ARCH:-amd64}"
PKG_NAME="unpackr_${VERSION}_${ARCH}"
STAGE_DIR="/tmp/${PKG_NAME}"
DIST_DIR="${DIST_DIR:-$(pwd)/dist}"
BIN_SOURCE="${BIN_SOURCE:-target/release/unpackr}"

if [ ! -f "${BIN_SOURCE}" ]; then
    echo "Compiling release binary..."
    cargo build --release
fi

echo "Staging Debian package files in ${STAGE_DIR}..."
rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}/DEBIAN"
mkdir -p "${STAGE_DIR}/usr/bin"
mkdir -p "${STAGE_DIR}/usr/share/applications"
mkdir -p "${STAGE_DIR}/usr/share/icons/hicolor/scalable/apps"
mkdir -p "${STAGE_DIR}/usr/share/bash-completion/completions"
mkdir -p "${STAGE_DIR}/usr/share/zsh/site-functions"
mkdir -p "${STAGE_DIR}/usr/share/fish/vendor_completions.d"
mkdir -p "${STAGE_DIR}/usr/share/doc/unpackr"

# Binary (stripped)
install -m 755 "${BIN_SOURCE}" "${STAGE_DIR}/usr/bin/unpackr"
strip -s "${STAGE_DIR}/usr/bin/unpackr"

# Desktop integration
install -m 644 extra/unpackr.desktop "${STAGE_DIR}/usr/share/applications/unpackr.desktop"
install -m 644 extra/unpackr.svg "${STAGE_DIR}/usr/share/icons/hicolor/scalable/apps/unpackr.svg"

# Documentation
install -m 644 README.md "${STAGE_DIR}/usr/share/doc/unpackr/README.md"
cat << 'EOF' > "${STAGE_DIR}/usr/share/doc/unpackr/copyright"
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: unpackr
Upstream-Contact: Rahul Kumar <chhonkarrahul1362@gmail.com>
Source: https://github.com/RahulKumar-007/Unpackr

Files: *
Copyright: 2026 Rahul Kumar
License: MIT or Apache-2.0
EOF

# Shell auto-completions
"${BIN_SOURCE}" completions bash > "${STAGE_DIR}/usr/share/bash-completion/completions/unpackr"
"${BIN_SOURCE}" completions zsh > "${STAGE_DIR}/usr/share/zsh/site-functions/_unpackr"
"${BIN_SOURCE}" completions fish > "${STAGE_DIR}/usr/share/fish/vendor_completions.d/unpackr.fish"

# Calculate installed size in KB
INSTALLED_SIZE=$(du -sk "${STAGE_DIR}" | cut -f1)

# Control file
cat << EOF > "${STAGE_DIR}/DEBIAN/control"
Package: unpackr
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Installed-Size: ${INSTALLED_SIZE}
Maintainer: Rahul Kumar <chhonkarrahul1362@gmail.com>
Homepage: https://github.com/RahulKumar-007/Unpackr
Description: Low-disk-space archive extraction engine and native GUI
 Unpackr is a production-quality, low-disk-space archive extraction engine
 and native desktop GUI. It eliminates the classic 2x disk capacity requirement
 by progressively punching filesystem holes in already-extracted archive payload
 blocks in-place while strictly preserving ZIP structure, CRC-32 integrity,
 and crash resumability.
EOF

mkdir -p "${DIST_DIR}"
echo "Building Debian package with dpkg-deb..."
dpkg-deb --build --root-owner-group "${STAGE_DIR}" "${DIST_DIR}/${PKG_NAME}.deb"

echo "Package created successfully: ${DIST_DIR}/${PKG_NAME}.deb"
ls -lh "${DIST_DIR}/${PKG_NAME}.deb"
dpkg-deb --info "${DIST_DIR}/${PKG_NAME}.deb"
