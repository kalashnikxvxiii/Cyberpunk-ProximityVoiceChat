#!/usr/bin/env bash
# Downloads Valve's Steam Audio SDK (Apache 2.0) into third_party/steamaudio:
# the Windows DLL + import library the plugin links, the Linux library the
# tests run against, the headers and the third-party licences. Pinned and
# checksummed: the bindings (audionimbus-sys =4.8.1) must match this exact
# version, or calls would go to the wrong fields.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=4.8.1
SHA256=4a0aa5ec1176f38f0b0993a37c2259d9e86f27e22d5e24f83ec4c3cb9a1d5449
LICENSE_SHA256=cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30
dest=third_party/steamaudio
[ -e "$dest/.version" ] && [ "$(cat "$dest/.version")" = "$VERSION" ] && exit 0
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/sa.zip" "https://github.com/ValveSoftware/steam-audio/releases/download/v$VERSION/steamaudio_$VERSION.zip"
echo "$SHA256  $tmp/sa.zip" | sha256sum -c --quiet
curl -fsSL -o "$tmp/LICENSE.md" "https://raw.githubusercontent.com/ValveSoftware/steam-audio/v$VERSION/LICENSE.md"
echo "$LICENSE_SHA256  $tmp/LICENSE.md" | sha256sum -c --quiet
unzip -q "$tmp/sa.zip" 'steamaudio/lib/windows-x64/phonon.*' 'steamaudio/lib/linux-x64/libphonon.so' \
  'steamaudio/include/*' 'steamaudio/THIRDPARTY.md' -d "$tmp"
rm -rf "$dest" && mkdir -p "$dest"
cp -r "$tmp/steamaudio/lib" "$tmp/steamaudio/include" "$tmp/steamaudio/THIRDPARTY.md" "$tmp/LICENSE.md" "$dest/"
echo "$VERSION" > "$dest/.version"
echo "→ $dest (Steam Audio $VERSION)"
