#!/usr/bin/env bash
# Copies the built plugin and the CET mod into the game. Override the path
# with CP2077_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
game="${CP2077_DIR:-$HOME/Games/Heroic/Cyberpunk 2077}"
dest="$game/red4ext/plugins/VoiceChat"
cet="$game/bin/x64/plugins/cyber_engine_tweaks/mods/VoiceChat"
# Overwriting the DLL in place while the game runs changes the code it has
# mapped and crashes it. Refuse, and replace by rename (new inode) anyway.
if pgrep -fi 'Cyberpunk2077\.exe' >/dev/null; then
  echo "Cyberpunk 2077 is running: close it before installing." >&2
  exit 1
fi
mkdir -p "$dest"
cp target/x86_64-pc-windows-msvc/release/VoiceChat.dll "$dest/VoiceChat.dll.new"
mv -f "$dest/VoiceChat.dll.new" "$dest/VoiceChat.dll"
# Steam Audio (loaded by full path from here, see sound3d.rs).
phonon=third_party/steamaudio/lib/windows-x64/phonon.dll
if ! cmp -s "$phonon" "$dest/phonon.dll"; then
  cp "$phonon" "$dest/phonon.dll.new"
  mv -f "$dest/phonon.dll.new" "$dest/phonon.dll"
fi
# First install: take the config from the latest package, if any.
if [ ! -e "$dest/config.ini" ] && [ -e dist/config.ini ]; then cp dist/config.ini "$dest/"; fi
if [ -d "$(dirname "$cet")" ]; then
  # Only our code: CET keeps the mod's logs and db.sqlite3 in the same folder.
  rm -rf "$cet/modules" "$cet/init.lua"
  mkdir -p "$cet"
  cp -r cet/VoiceChat/. "$cet/"
  echo "→ $cet"
else
  echo "CET not found: settings window, HUD icon and walls/reverb need Cyber Engine Tweaks." >&2
fi
echo "→ $dest/VoiceChat.dll"
