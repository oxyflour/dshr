#!/usr/bin/env bash
# M4 G1：校验归档 SHA-256、解压、跑包内官方 sidecar 与插件自检，再清理临时目录。
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
[[ -x "$TMP/dshr" ]] || { echo "missing dshr in archive" >&2; exit 1; }
[[ -x "$TMP/runtime/node/node" ]] || { echo "missing bundled Node" >&2; exit 1; }
for asset in probe-sidecar.mjs probe-plugin.mjs cordis; do
  if [[ -e "$TMP/sidecar/$asset" ]]; then
    echo "diagnostic sidecar asset in desktop package: $asset" >&2
    exit 1
  fi
done
cd "$TMP"
unset DSHR_NODE DSHR_RUNTIME_DIR DSHR_APP_ROOT
export DSHR_HOME="$TMP/.home"
export PATH=/usr/bin:/bin
./runtime/node/node --version
./dshr --selftest-official
./dshr --selftest-cpp-component
./dshr --selftest-cpp-install
./dshr --selftest-cpp-cli-install
echo "[verify-package] PASS $ARCHIVE"
