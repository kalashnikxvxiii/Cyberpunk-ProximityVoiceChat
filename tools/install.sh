#!/usr/bin/env bash
# Copies the built plugin into the game. Override the path with CP2077_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
game="${CP2077_DIR:-$HOME/Games/Heroic/Cyberpunk 2077}"
dest="$game/red4ext/plugins/VoiceChat"
mkdir -p "$dest"
# Overwriting the DLL in place while the game runs changes the code it has
# mapped and crashes it. Refuse, and replace by rename (new inode) anyway.
if pgrep -fi 'Cyberpunk2077\.exe' >/dev/null; then
  echo "Cyberpunk 2077 is running: close it before installing." >&2
  exit 1
fi
cp target/x86_64-pc-windows-msvc/release/VoiceChat.dll "$dest/VoiceChat.dll.new"
mv -f "$dest/VoiceChat.dll.new" "$dest/VoiceChat.dll"
# First install: take the config from the latest package, if any.
if [ ! -e "$dest/config.ini" ] && [ -e dist/config.ini ]; then cp dist/config.ini "$dest/"; fi
echo "→ $dest/VoiceChat.dll"
