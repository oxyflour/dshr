#!/usr/bin/env bash
# M4 G1：校验归档 SHA-256、解压、跑包内 sidecar/official 自检，再清理临时目录。
set -euo pipefail

ARCHIVE="${1:-dist/dshr-debug-linux-x86_64.tar.gz}"
ARCHIVE="$(cd "$(dirname "$ARCHIVE")" && pwd)/$(basename "$ARCHIVE")"
HASH_FILE="$ARCHIVE.sha256"
[[ -f "$HASH_FILE" ]] || { echo "missing $HASH_FILE" >&2; exit 1; }

(cd "$(dirname "$ARCHIVE")" && sha256sum -c "$(basename "$HASH_FILE")")
echo "[verify-package] sha256 ok"

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT
tar -xzf "$ARCHIVE" -C "$TMP"
[[ -x "$TMP/chat" ]] || { echo "missing chat in archive" >&2; exit 1; }
cd "$TMP"
export DSHR_HOME="$TMP/.home"
./chat --selftest-sidecar-recovery
./chat --selftest-official
echo "[verify-package] PASS $ARCHIVE"
