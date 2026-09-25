#!/usr/bin/env bash
set -euo pipefail

DIST_DIR="${DIST_DIR:-$(pwd)/dist}"
BIN_SOURCE="${BIN_SOURCE:-target/release/unpackr}"

if [ ! -f "${BIN_SOURCE}" ]; then
    echo "Compiling release binary..."
    cargo build --release
fi

echo "Generating shell completion scripts..."
mkdir -p target/completions
"${BIN_SOURCE}" completions bash > target/completions/unpackr
"${BIN_SOURCE}" completions zsh > target/completions/_unpackr
"${BIN_SOURCE}" completions fish > target/completions/unpackr.fish

echo "Generating RPM package..."
cargo generate-rpm

mkdir -p "${DIST_DIR}"
# Find the generated rpm in target/generate-rpm/
RPM_FILE=$(find target/generate-rpm -name "*.rpm" -type f | head -n 1)

if [ -n "${RPM_FILE}" ]; then
    cp "${RPM_FILE}" "${DIST_DIR}/"
    RPM_BASENAME=$(basename "${RPM_FILE}")
    echo "RPM created successfully: ${DIST_DIR}/${RPM_BASENAME}"
    ls -lh "${DIST_DIR}/${RPM_BASENAME}"
else
    echo "Error: RPM file not found in target/generate-rpm"
    exit 1
fi
