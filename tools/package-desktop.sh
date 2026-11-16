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

# 随包运行时：vendor node + probe sidecar（不含 dsh-official/node_modules）。
NODE_BIN="${NODE_BIN:-$(command -v node || true)}"
if [[ -n "$NODE_BIN" && -x "$NODE_BIN" ]]; then
  mkdir -p "$STAGE/runtime/node"
  cp "$NODE_BIN" "$STAGE/runtime/node/node"
  chmod +x "$STAGE/runtime/node/node"
else
  echo "[package-desktop] warning: no node binary; package will be Native-only" >&2
fi
mkdir -p "$STAGE/sidecar"
for name in probe-sidecar.mjs probe-plugin.mjs; do
  [[ -f "$ROOT/sidecar/$name" ]] && cp "$ROOT/sidecar/$name" "$STAGE/sidecar/$name"
done
if [[ -d "$ROOT/sidecar/dsh-official" ]]; then
  cp -a "$ROOT/sidecar/dsh-official" "$STAGE/sidecar/dsh-official"
fi
COMPONENT="$STAGE/components/cpp-component"
mkdir -p "$COMPONENT"
if [[ -f "$ROOT/crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm" ]]; then
  cp "$ROOT/crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm" "$COMPONENT/plugin.wasm"
  cat > "$COMPONENT/plugin.toml" <<'EOF'
[plugin]
id = "dshr.selftest.component"
version = "0.1.0"
api = "cordis-portable/v1"
description = "cpp-v1 wasm component selftest"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
timeout_ms = 30000
EOF
  cat > "$COMPONENT/package.json" <<'EOF'
{
  "name": "dshr.selftest.component",
  "version": "0.1.0",
  "private": true
}
EOF
fi
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo dirty)"
cat > "$STAGE/README.txt" <<EOF
dshr $OS package
configuration: $CONFIG
commit: $COMMIT
built: $(date -u +%Y-%m-%dT%H:%M:%SZ)

Run chat --selftest-adc-panel / --selftest-recovery / --selftest-crash-recovery / --selftest-cpp-component.
Includes runtime/node, probe sidecar, vendored dsh-official sidecar, and components/cpp-component.
No installer, code signing, or auto-update in this package.
EOF

ARCHIVE="$ROOT/dist/dshr-$CONFIG-$OS-$ARCH.tar.gz"
rm -f "$ARCHIVE"
tar -C "$STAGE" -czf "$ARCHIVE" .
sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
echo "[package-desktop] $ARCHIVE"
echo "[package-desktop] sha256=$(cut -d' ' -f1 "$ARCHIVE.sha256")"
