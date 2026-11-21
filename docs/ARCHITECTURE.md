# Architecture

## How a session runs

1. **The game** (game thread, `crates/plugin/src/coop.rs`) looks for `NightCoop.dll` and its SDK (C ABI,
   version 1, header in `third_party/nightcoop-sdk/`) every second until it finds it, and registers the mod
   (`kalashnikxv.voicechat`, protocol 1, channels and shared state only). From then on the chat runs in
   NightCoop's frame (`crates/coop/src/chat.rs`); it also reads the player's position and camera direction.
2. **The room key**: the host of each session draws 32 random bytes and puts them in the mod's shared state
   (`vc.key`). NightCoop hands them only to partners with the mod, late joiners too, and removes them when
   the session ends. Guests can't write anything (`stateRequest` refuses everything).
3. **The voice**: the audio thread queues 20 ms Opus frames; every 40 ms an *unreliable* message carries the
   queued ones (2, or 3 after an `E_LIMIT`), sealed with the room key and bound to sender and sequence. The
   speaker chooses who hears them: partners within 72 m, further when shouting (up to 10x; from their body in
   our world, `partnerBody` → `FindEntityByID`; unknown position = everybody), and their own party at any
   distance. With 4 or more recipients a single `TO_ALL` goes out, and receivers filter by distance.
   Positions don't travel. The party is announced with a *reliable* message when it changes. Silence = no
   messages.
4. **After a stall** (loading, 0.5–1.5 s frames) only the last ~100 ms are kept, on both sides, and playback
   starts over: latency doesn't pile up.
5. **The session manager** (`session.rs`) only says what is going on: NightCoop (host/guest), **manual**
   (server and room from the settings, server key pinned on first connection) or **offline** with the
   reason. If NightCoop is there but its SDK is missing or switched off, the chat stays off: it never works
   around the player's choices with a network of its own.

Cost measured on the fake NightCoop (`crates/coop/src/fake.rs`, 4 people talking): no limit exceeded; added
latency at the 95th percentile 25 ms at 144 fps, 40 at 60 fps, 70 at 30 fps (40 ms batch + one listener
frame).

## Security

| Threat | Defence |
|---|---|
| Eavesdropping on the network | NightCoop's links are encrypted; on top, voice is sealed with the room key (`protocol/e2e.rs`) |
| Strangers in the chat | The key reaches only session partners with the mod; a new one for every session |
| A guest posing as another | NightCoop's `from` names the sender, and the seal is bound to it: a guest can't pass voice off as another's. The host is trusted here: it holds the room key and relays between guests, so it could seal voice under any guest's id |
| Replay, old packets | Per-sender sequence: only newer data is accepted |
| Malformed data from a partner | Sizes, frame count and version checked; per-sender limit; text passed through `cleanText`; no panics (fuzzed with random data) |
| A guest writing to the shared state | `stateRequest` refuses everything |
| Fake server (manual) | Pinned key fingerprint (`#FINGERPRINT` or TOFU), warnings in Diagnostics |
| The relay guessing the room code (manual) | Voice keys come from the room code through Argon2id (32 MiB, 3 passes); "New random code" draws 80 bits, and the menu warns about codes under 12 characters |
| Server abuse (manual) | Per-IP, global and per-room limits; names cleaned and capped |

Remaining trust: the session's host sees all of the mod's data (it holds the key) and could impersonate a
guest. In manual mode, the room code is the key: anyone who knows it can listen and talk. The mod exposes no IP
address: in a NightCoop session it opens no ports and talks to nobody except through NightCoop.

## Audio (`crates/dsp`, `plugin/audio.rs`)

Microphone → gain → AEC3 (reference: **only** the other players' voices) / noise suppression (WebRTC or
RNNoise) → push-to-talk or threshold → Opus 24 kbps → NightCoop channels (or, in manual mode, E2E seal →
server). Receiving → jitter buffer → Opus (with packet loss concealment) → low-pass filter and gains from the
acoustic path → reverb (the speaker's and the listener's surroundings) → output. In the lobby: plain voice.

The game's own sound (`plugin/gamemix.rs`): the game's WASAPI render stream is read inside the game process
(only streams created by the game executable or XAudio2, never other programs) and analysed by
`vc_dsp::scene` for noise level and kind; nothing is recorded or sent.

## Acoustics

**Steam Audio** (`crates/acoustics`, `plugin/sound3d.rs`, `plugin/rays.rs`): Valve's 3D audio library
(Apache 2.0, `phonon.dll` 4.8.1, downloaded and verified by `tools/fetch-steamaudio.sh`). The geometry is the
game's: Steam Audio asks for rays, the DLL casts them with the game's physics (`SyncRaycastByCollisionGroup`,
groups Static, Terrain, Destructible, Dynamic, Vehicle) and reports the material (`concrete.physmat`, ...).
- Direct sound (what hides the voice, what it goes through): a few single rays, on the game thread every
  100 ms.
- Rebounds (2048 rays, 2 bounces): on the `voicechat-acoustics` thread; the rays wait for the game thread,
  which casts them for at most 2 ms per frame.
- Rendering: direct effect (occlusion, per-material transmission, air), HRTF; rebounds in first-order
  ambisonics per voice, decoded with the HRTF. The distance law (range, shouting, noise, party radio) stays
  ours (`vc_dsp::spatial`).
- Collision seams: single rays count as blocked if copies moved 8 cm to both sides are. Closed doors
  (Dynamic objects) let 10% of the rebounds through; grates and fences 70%.
- `phonon.dll` is loaded late and by full path: if it's missing, the plugin starts with the classic model.
Tests: `cargo test -p vc-acoustics` (box scenes against the real Steam Audio: shop window, doors, seams,
tunnel, steps, both threads together, HRTF volume).

**Classic model** (`cet/VoiceChat/modules/acoustics.lua`, when Steam Audio is off or missing): raycasts from
CET: direct path, through openings in walls (more than one too), around corners; with no air path the voice
only goes through thin walls. The room reverb (for your own voice too) always comes from here: 21 rays
weighted by distance and cover. Offline test: `luajit cet/tests/acoustics_test.lua`.

## Diagnostics

`plugin/events.rs`: every problem has a phase, a detail and a practical hint; repeats are grouped, solved
ones disappear; it also flags a push-to-talk key NightCoop already uses (`keysUsed`: NightCoop doesn't
consume keys). At the top of the tab: NightCoop's build, who has the mod and the voice measurements (latency
from `sharedTimeMs`, jitter, loss, drops, `E_LIMIT`, stalls). Shown in CET > VoiceChat > Diagnostics, also
written to the log `red4ext/logs/voicechat-*.log`.

Test tools (test voice, echo, environment diagnostics) are not in the release: `tools/debug/`.
