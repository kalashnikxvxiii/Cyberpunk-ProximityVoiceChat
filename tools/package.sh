#!/usr/bin/env bash
# Builds the zip to hand to other players.
#   tools/package.sh SERVER[:PORT] [ROOM]
# ROOM defaults to a random code; everyone must use the same one.
set -euo pipefail
cd "$(dirname "$0")/.."

server="${1:?usage: tools/package.sh SERVER[:PORT] [ROOM]}"
[[ "$server" == *:* ]] || server="$server:47900"
random=$(head -c 512 /dev/urandom | LC_ALL=C tr -dc 'A-Z2-9')
room="${2:-${random:0:6}}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

tools/build-plugin.sh >/dev/null

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
dir="$stage/red4ext/plugins/VoiceChat"
mkdir -p "$dir"
cp target/x86_64-pc-windows-msvc/release/VoiceChat.dll "$dir/"
sed -e "s|@SERVER@|$server|" -e "s|@ROOM@|$room|" tools/package/config.ini >"$dir/config.ini"
sed -e "s|@VERSION@|$version|" tools/package/LEGGIMI.txt >"$stage/VoiceChat-LEGGIMI.txt"
# Windows line endings for Notepad.
sed -i 's/$/\r/' "$dir/config.ini" "$stage/VoiceChat-LEGGIMI.txt"

mkdir -p dist
out="$PWD/dist/VoiceChat-$version.zip"
rm -f "$out"
(cd "$stage" && zip -qr "$out" .)
cp "$dir/config.ini" dist/config.ini
echo "→ $out (server $server, room $room)"
