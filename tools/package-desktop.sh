#!/usr/bin/env bash
# M4 G1 最小 Linux/macOS 打包：编译 dshr，连同 README/版本信息压成 tar.gz。
# 不包含安装器、签名、自动更新；这些仍属后续发布工作。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
CONFIG="${1:-debug}"
if [[ "$CONFIG" != "debug" && "$CONFIG" != "release" ]]; then
  echo "usage: tools/package-desktop.sh [debug|release]" >&2
  exit 2
fi

NODE_BIN="${NODE_BIN:-$(command -v node || true)}"
if [[ -z "$NODE_BIN" || ! -x "$NODE_BIN" ]]; then
  echo "missing Node executable; set NODE_BIN before packaging" >&2
  exit 1
fi
OFFICIAL="$ROOT/sidecar/dsh-official"
for file in sidecar-dsh.mjs node_modules/@deepseek-ai/{cordis,dsh-tool-fs,dsh-tool-workspace-dependencies}/package.json; do
  if [[ ! -f "$OFFICIAL/$file" ]]; then
    echo "missing required runtime asset: $OFFICIAL/$file" >&2
    exit 1
  fi
done

ARGS=(build -p dshr-desktop-gpui --bin dshr)
if [[ "$CONFIG" == "release" ]]; then ARGS+=(--release); fi
CARGO_BIN="${CARGO:-cargo}"
"$CARGO_BIN" "${ARGS[@]}"

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="$TARGET_DIR/$CONFIG/dshr"
if [[ ! -x "$BIN" ]]; then echo "missing $BIN" >&2; exit 1; fi

OS="${OS_OVERRIDE:-$(uname -s | tr '[:upper:]' '[:lower:]')}"
ARCH="$(uname -m)"
STAGE="$ROOT/dist/dshr-$CONFIG-$OS-$ARCH"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/dshr"

if [[ "$OS" == "darwin" ]]; then
  APP="$STAGE/dshr.app"
  mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
  cp "$BIN" "$APP/Contents/MacOS/dshr"
  cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.dshr.desktop</string>
  <key>CFBundleName</key><string>dshr</string>
  <key>CFBundleDisplayName</key><string>dshr</string>
  <key>CFBundleExecutable</key><string>dshr</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
  if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$APP"
    codesign --verify --strict --verbose=2 "$APP"
  else
    echo "[package-desktop] warning: dshr.app is unsigned (MACOS_SIGN_IDENTITY not set)" >&2
  fi
fi

# 随包运行时：vendor node 与官方 DSH sidecar。
mkdir -p "$STAGE/runtime/node"
cp "$NODE_BIN" "$STAGE/runtime/node/node"
chmod +x "$STAGE/runtime/node/node"
mkdir -p "$STAGE/sidecar"
cp -a "$OFFICIAL" "$STAGE/sidecar/dsh-official"
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

Run dshr --selftest-adc-panel / --selftest-recovery / --selftest-crash-recovery / --selftest-cpp-component.
Includes runtime/node, vendored dsh-official sidecar, and components/cpp-component.
No installer, code signing, or auto-update in this package.
EOF

ARCHIVE="$ROOT/dist/dshr-$CONFIG-$OS-$ARCH.tar.gz"
rm -f "$ARCHIVE"
tar -C "$STAGE" -czf "$ARCHIVE" .
sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
echo "[package-desktop] $ARCHIVE"
echo "[package-desktop] sha256=$(cut -d' ' -f1 "$ARCHIVE.sha256")"
