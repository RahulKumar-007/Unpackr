#!/usr/bin/env bash
set -euo pipefail

MANIFEST="$(dirname "$0")/io.github.rahulkumar_007.Unpackr.yml"
BUILD_DIR="/tmp/flatpak-unpackr-build"
REPO_DIR="/tmp/flatpak-unpackr-repo"

if ! command -v flatpak-builder &>/dev/null; then
    echo "flatpak-builder is not installed. Install via: sudo apt install flatpak-builder"
    exit 1
fi

flatpak-builder --force-clean --repo="${REPO_DIR}" "${BUILD_DIR}" "${MANIFEST}"
echo "Flatpak built successfully in ${REPO_DIR}"
