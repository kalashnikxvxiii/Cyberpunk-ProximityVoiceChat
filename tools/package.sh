#!/usr/bin/env bash
# Builds the release zips, the same for everybody (no addresses inside):
#   dist/VoiceChat-<version>.zip        the mod (extract into the game folder)
#   dist/VoiceChat-relay-<version>.zip  the optional relay server
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
tools/build-plugin.sh >/dev/null
# Keep build machine paths (home directory, user name) out of the binaries.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~ --remap-path-prefix=$PWD=."
cargo build --release -q -p vc-server
cargo xwin build --release -q --target x86_64-pc-windows-msvc -p vc-server 2>/dev/null

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
crlf() { sed -e "s|@VERSION@|$version|" "$1" | sed 's/$/\r/' >"$2"; }

mod="$stage/mod"
mkdir -p "$mod/red4ext/plugins/VoiceChat" "$mod/bin/x64/plugins/cyber_engine_tweaks/mods"
cp target/x86_64-pc-windows-msvc/release/VoiceChat.dll "$mod/red4ext/plugins/VoiceChat/"
cp third_party/steamaudio/lib/windows-x64/phonon.dll "$mod/red4ext/plugins/VoiceChat/"
cp -r cet/VoiceChat "$mod/bin/x64/plugins/cyber_engine_tweaks/mods/VoiceChat"
crlf tools/package/README.txt "$mod/VoiceChat-README.txt"
crlf tools/package/LEGGIMI.txt "$mod/VoiceChat-LEGGIMI.txt"
crlf LICENSE "$mod/VoiceChat-LICENSE.txt"
{
  python3 -I tools/notices.py x86_64-pc-windows-msvc voice-chat
  # The SDK bindings come from NightCoop's header (MIT).
  printf '\n%s\nNightCoop SDK header (third_party/nightcoop-sdk)\n%s\n' "$(printf '=%.0s' {1..72})" "$(printf '=%.0s' {1..72})"
  cat third_party/nightcoop-sdk/LICENSE
  # phonon.dll: Valve's Steam Audio (Apache 2.0), and what it contains.
  printf '\n%s\nSteam Audio (phonon.dll)\n%s\n' "$(printf '=%.0s' {1..72})" "$(printf '=%.0s' {1..72})"
  cat third_party/steamaudio/LICENSE.md
  cat third_party/steamaudio/THIRDPARTY.md
} | sed 's/$/\r/' >"$mod/VoiceChat-THIRD-PARTY-NOTICES.txt"

relay="$stage/relay/VoiceChat-relay"
mkdir -p "$relay"
cp target/release/vc-server "$relay/vc-server"
cp target/x86_64-pc-windows-msvc/release/vc-server.exe "$relay/vc-server.exe"
crlf tools/package/README-relay.txt "$relay/README.txt"
crlf tools/package/LEGGIMI-relay.txt "$relay/LEGGIMI.txt"
crlf LICENSE "$relay/LICENSE.txt"
python3 -I tools/notices.py x86_64-pc-windows-msvc vc-server | sed 's/$/\r/' >"$relay/THIRD-PARTY-NOTICES.txt"

mkdir -p dist
rm -f "dist/VoiceChat-$version.zip" "dist/VoiceChat-relay-$version.zip"
(cd "$mod" && zip -qr "$OLDPWD/dist/VoiceChat-$version.zip" .)
(cd "$stage/relay" && zip -qr "$OLDPWD/dist/VoiceChat-relay-$version.zip" .)
# For friends: just the mod. For Nexus: both files and the page texts.
rm -rf dist/amici dist/nexus
mkdir -p dist/amici dist/nexus/files dist/nexus/page
cp "dist/VoiceChat-$version.zip" dist/amici/
cp "dist/VoiceChat-$version.zip" "dist/VoiceChat-relay-$version.zip" dist/nexus/files/
for f in tools/nexus/*; do sed -e "s|@VERSION@|$version|g" "$f" >"dist/nexus/page/$(basename "$f")"; done
echo "→ dist/amici/VoiceChat-$version.zip"
echo "→ dist/nexus/ (files/ + page/)"
