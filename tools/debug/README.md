# Test tools (left out of the release)

Removed on 2026-11-21 before the Nexus release. `debug-tools.patch` brings all of them back:

```sh
git apply tools/debug/debug-tools.patch      # put them back
git apply -R tools/debug/debug-tools.patch   # remove them again
```

If the code has changed in the meantime and the patch no longer applies: `git apply --3way`, or
`git apply --reject` and fix the `.rej` files by hand. Never ship a build with the patch applied.

What it puts back:

- **Test voice** (`crates/plugin/src/testvoice.rs`, exports `VoiceChat_Test` / `VoiceChat_TestStatus`,
  CET Environment tab): records up to 10 s of your own voice and places it in the world as a pretend player
  (id 65534) with a simulated network (delay, loss); distance on screen with the menu closed.
- **Echo through NightCoop** (`Game::echo` in `crates/coop/src/chat.rs`): your voice goes the whole way a
  partner's does except the network, and comes back from where you spoke (`ECHO_PEER` = 65535).
- **Echo from the server** (`vc-server --echo`, `ECHO_ID` in `vc-protocol`).
- **Environment diagnostics** (CET): measured reverb/size/walls/sky/furniture, game sound, noise and scene
  kind, personal sound checks, ear height, "What's in front of me?" (collision groups in front of the
  camera), the path of each voice ("direct", "wall 0.30 m", "Steam Audio: ..."). On the DLL side the status
  keys `noise_db`, `scene_kind`, `personal`, `game_sound`, `loud_db`, `wet_used`, `acoustics`,
  `acoustics_stats` and the 7th field of `VoiceChat_Speakers`.
- **Steam Audio logs**: probe rays at the first voice, counters and a line per voice every 10 s, the test
  voice's line every 2 s.
- `cet/tests/acoustics_test.lua` prints each case's path again.

Test plans that use these tools: `TEST-PLAN.md` (alone: V, S, E) and `TWO-PLAYER-TEST.txt` (with a second
player). They quote the English UI; with Italian selected the labels are the Italian ones.
