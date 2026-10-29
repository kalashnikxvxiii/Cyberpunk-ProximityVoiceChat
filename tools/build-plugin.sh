#!/usr/bin/env bash
# Cross-compiles VoiceChat.dll from Linux with cargo-xwin.
# RED4ext.SDK has a few #includes whose case differs from the files on disk
# (fine on Windows, fatal on Linux): we build an overlay dir of symlinks with
# the expected names and add it to the include path of clang-cl and bindgen.
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=x86_64-pc-windows-msvc
cargo fetch --target "$TARGET" >/dev/null

sdk_root=$(cargo metadata --format-version 1 --filter-platform "$TARGET" |
  python3 -c 'import json,sys,os; m=json.load(sys.stdin); print(next(os.path.dirname(p["manifest_path"]) for p in m["packages"] if p["name"]=="red4ext-rs"))')
inc="$sdk_root/deps/RED4ext.SDK/include"
overlay="$PWD/target/case-overlay"
rm -rf "$overlay" && mkdir -p "$overlay"

grep -rhoE '#include [<"]RED4ext/[^>"]+' "$sdk_root/deps" | sed -E 's/#include [<"]//' | sort -u |
while read -r rel; do
  [ -e "$inc/$rel" ] && continue
  real=$(cd "$inc" && find RED4ext -ipath "$rel" -print -quit)
  [ -n "$real" ] || { echo "header not found: $rel" >&2; exit 1; }
  mkdir -p "$overlay/$(dirname "$rel")"
  ln -s "$inc/$real" "$overlay/$rel"
done

# cargo-xwin sets CXXFLAGS_<target> itself, so pass the overlay via CPATH.
export CPATH="$overlay"
export BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_msvc="-I$overlay"
cargo xwin build --release --target "$TARGET" -p voice-chat "$@"
echo "→ target/$TARGET/release/VoiceChat.dll"
