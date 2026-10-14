#!/usr/bin/env bash
# Launch benilla against the local zhCN install, for eyeballing the CJK text.
#
# The binary is the `play` profile's, not `debug` or `release`: `target/play/benilla.exe`.
# Build it first with `cargo build --profile play -p benilla` (~40 min cold).
#
#   scripts/play-cn.sh [extra args...]
#
# WOW_DATA may be overridden; the default is this machine's Chinese install.
#
# Fonts: the install's own loose `Fonts\` folder (beside `Data`) is read BEFORE the MPQ archives,
# which is how the 1.12 client lets a player swap the typeface. So a `.ttf` dropped there under an
# archive font's name replaces it: `Fonts\fzxhljw.ttf` is the face the CJK fallback picks on this
# install, and deleting a loose file falls back to `fonts.MPQ`'s own.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
exe="$root/target/play/benilla.exe"
: "${WOW_DATA:=D:\\Games\\WoW 1.12\\Data}"
export WOW_DATA

[ -x "$exe" ] || {
    echo "play-cn: no $exe — build it first:" >&2
    echo "  cargo build --profile play -p benilla" >&2
    exit 1
}

echo "play-cn: WOW_DATA=$WOW_DATA"
exec "$exe" "$@"
