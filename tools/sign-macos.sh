#!/usr/bin/env bash
# M4 G1 macOS 签名/公证的最小入口（只在 macOS runner 上运行）。
#
# 用法：
#   tools/sign-macos.sh <path> [identity]
# 环境：
#   MACOS_SIGN_IDENTITY   codesign identity（默认取第二个参数）
#   MACOS_NOTARY_PROFILE  notarytool profile（可选；不设则只做 codesign）
#
# 无证书时明确失败，不能把 unsigned 当成已签名。
set -euo pipefail

TARGET="${1:-}"
IDENTITY="${2:-${MACOS_SIGN_IDENTITY:-}}"
if [[ -z "$TARGET" || -z "$IDENTITY" ]]; then
  echo "usage: tools/sign-macos.sh <path> [identity]" >&2
  exit 2
fi
if [[ ! -e "$TARGET" ]]; then
  echo "missing target: $TARGET" >&2
  exit 1
fi

codesign --force --options runtime --timestamp --sign "$IDENTITY" "$TARGET"
codesign --verify --strict --verbose=2 "$TARGET"
echo "[sign-macos] codesign ok: $TARGET"

if [[ -n "${MACOS_NOTARY_PROFILE:-}" ]]; then
  echo "[sign-macos] notarization requires an archive/app bundle; not implemented for bare binary" >&2
fi
