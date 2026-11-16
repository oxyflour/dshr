#!/usr/bin/env bash
# M4 G1 最小 Linux/macOS 打包：编译 chat，连同 README/版本信息压成 tar.gz。
# 不包含安装器、签名、自动更新；这些仍属后续发布工作。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
CONFIG="${1:-debug}"
if [[ "$CONFIG" != "debug" && "$CONFIG" != "release" ]]; then
  echo "usage: tools/package-desktop.sh [debug|release]" >&2
  exit 2
fi

ARGS=(build -p dshr-desktop-gpui --bin chat)
if [[ "$CONFIG" == "release" ]]; then ARGS+=(--release); fi
CARGO_BIN="${CARGO:-cargo}"
"$CARGO_BIN" "${ARGS[@]}"

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="$TARGET_DIR/$CONFIG/chat"
if [[ ! -x "$BIN" ]]; then echo "missing $BIN" >&2; exit 1; fi

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
STAGE="$ROOT/dist/dshr-$CONFIG-$OS-$ARCH"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/chat"
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo dirty)"
cat > "$STAGE/README.txt" <<EOF
dshr $OS package
configuration: $CONFIG
commit: $COMMIT
built: $(date -u +%Y-%m-%dT%H:%M:%SZ)

Run chat --selftest-adc-panel / --selftest-recovery / --selftest-crash-recovery.
No installer, code signing, or auto-update in this package.
EOF

ARCHIVE="$ROOT/dist/dshr-$CONFIG-$OS-$ARCH.tar.gz"
rm -f "$ARCHIVE"
tar -C "$STAGE" -czf "$ARCHIVE" .
echo "[package-desktop] $ARCHIVE"
