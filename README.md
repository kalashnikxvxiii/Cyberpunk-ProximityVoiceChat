# VoiceChat (Cyberpunk 2077 · NightCoop)

[![CI](https://github.com/kalashnikxvxiii/Cyberpunk-ProximityVoiceChat/actions/workflows/ci.yml/badge.svg)](https://github.com/kalashnikxvxiii/Cyberpunk-ProximityVoiceChat/actions/workflows/ci.yml) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Proximity voice chat for [NightCoop](https://nightcoop.net/) co-op sessions: install it and it works.
Voice goes through NightCoop's channels (its SDK): no server, no addresses to exchange, no ports to open.

- Positional voice (distance, direction from the camera), walls and openings, room reverb.
- Listens to the game's own sound (only the game, inside its process): noise covers distant voices and
  lowers the reverb; paused during the pocket radio, calls and menus.
- Shouting carries the voice further (up to 10x), noise shortens it; glass panes and furniture.
- Optional realistic acoustics with Valve's Steam Audio: rebounds, materials, 3D sound.
- Party (radio at any distance), co-op lobby without distance.
- Echo cancellation (WebRTC AEC3), noise suppression (classic or RNNoise).
- Voice encrypted from player to player; only players in the session can join.
- Full CET menu, guided setup, HUD icon, problem register with hints. English and Italian.

## Install (players)

Requirements: Cyberpunk 2077 with [NightCoop](https://nightcoop.net/) (RED4ext comes with it) and, for the
menu, guided setup, HUD icon, walls and reverb, [Cyber Engine Tweaks](https://www.nexusmods.com/cyberpunk2077/mods/107).
Every player in the session needs VoiceChat, same version.

1. Close the game.
2. Extract `VoiceChat-<version>.zip` into the game folder (the one with `bin` and `red4ext`). Updating:
   overwrite; your settings in `red4ext\plugins\VoiceChat\config.ini` are kept.
3. Start the game and open the CET overlay: the guided setup starts (microphone, headphones, talk key,
   language).

Problems: CET overlay > VoiceChat > Diagnostics says what happened and what to do. Logs:
`<game>\red4ext\logs\voicechat-*.log`. Full player guide: [tools/package/README.txt](tools/package/README.txt).

## Privacy

- In a NightCoop session the mod has no network code of its own: voice goes through NightCoop's encrypted
  links, sealed again with a key the host draws for each session. No ports opened, no IP address shown.
  The session's host, as with every co-op mod, can see the mod's data.
- The microphone is sent only while you transmit; nothing is recorded or saved.
- The game's own sound is read inside the game process (never other programs) to measure noise; it is
  analysed on the spot, not recorded or sent.
- Manual mode (without NightCoop) connects only to the relay address you type in.

Architecture and threat model: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Reporting a vulnerability:
[SECURITY.md](SECURITY.md).

## Layout

```
crates/protocol/    packets, transport and player-to-player encryption
crates/coop/        NightCoop SDK (FFI), the chat over its channels, a fake NightCoop for tests
crates/server/      voice relay for manual mode: library + vc-server binary
crates/rendezvous/  secret of a hand-typed room (manual mode)
crates/dsp/         echo/noise, filters, reverb, spatialisation, jitter buffer
crates/acoustics/   Steam Audio bindings and rendering
crates/plugin/      VoiceChat.dll (RED4ext): audio, NightCoop, manual mode, API for CET
third_party/        NightCoop SDK header and license (MIT)
cet/VoiceChat/      CET mod: menu, guided setup, HUD, acoustics (raycasts)
tools/              build, install, packaging
tools/debug/        test tools left out of the release (a patch to re-apply) and test plans
deploy/             Docker for the optional relay server
```

## Build (Linux → Windows)

Requirements: `rustup` with the `x86_64-pc-windows-msvc` target, `cargo-xwin`, clang/lld, cmake, ninja,
luajit (tests).

```sh
cargo test                         # protocol, relay, sessions on a fake NightCoop, DSP, acoustics
luajit cet/tests/acoustics_test.lua
tools/build-plugin.sh              # → target/x86_64-pc-windows-msvc/release/VoiceChat.dll
tools/install.sh                   # into the game (CP2077_DIR to change the path), with the game closed
tools/package.sh                   # → dist/VoiceChat-<v>.zip and dist/VoiceChat-relay-<v>.zip
```

## Relay server (optional)

```sh
cargo run --release -p vc-server -- --bind 0.0.0.0:47900
docker compose -f deploy/compose.yaml up -d --build
```

Used in manual mode: CET > VoiceChat > General (`address:port#fingerprint`).

## License

MIT, see [LICENSE](LICENSE). Third-party code: NightCoop's SDK header (MIT, `third_party/nightcoop-sdk/`);
Valve's Steam Audio (Apache 2.0) is downloaded by `tools/fetch-steamaudio.sh`, not stored here. The release
zips carry every bundled library's license in `VoiceChat-THIRD-PARTY-NOTICES.txt`
(`tools/notices.py`).

NightCoop is an unofficial fan modification by its own authors; this mod only builds on its public SDK.
Not affiliated with CD PROJEKT RED.
