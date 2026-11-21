# In-game test plan

To run after every change to the NightCoop part. Apply `debug-tools.patch` first (see `README.md`) for the
test voice and echo sections. Log: `<game>/red4ext/logs/voicechat-*.log`. Labels below are the English UI.

## Alone

1. **Start with NightCoop.** In the log, in this order:
   - `VoiceChat 0.2.0 loaded`
   - `nightcoop: registered with the SDK (build …)`
   - `[info] Session: you are not in a NightCoop session`
   NightCoop's log lists the mod `kalashnikxv.voicechat`. In CET > VoiceChat > Diagnostics, at the top:
   "NightCoop", "Build: … (SDK v1)", "State: no session".
2. **Host a session alone.**
   - `[info] Session: NightCoop session (you are the host): voice goes through NightCoop`
   - `[info] Session: voice chat on, through NightCoop`
   In CET > VoiceChat > General: "NightCoop: host, 0 other players (0 with VoiceChat)". In Diagnostics the
   line "Voice: latency 0 ms, … 0 frames received …".
   While you talk the HUD icon lights up; no errors in the log (`entity_position`, `[error]`).
3. **NightCoop's push-to-talk key.** In General > Transmission pick one of NightCoop's keys (for example End
   or Page Up). Within a second, under the key: "NightCoop uses it too: …" (and in Diagnostics). Put the old
   key back: the warning goes away. In the log, `nightcoop: keys it reads:` lists the keys NightCoop
   declares: a key that isn't there gives no warning.
4. **Close the session.** `you are not in a NightCoop session` comes back; hosting another one, the chat
   turns itself back on.
5. **SDK off** (NightCoop's `sdk` setting off, restart):
   `[error] Session: NightCoop doesn't offer the SDK …`, chat off, no network of its own.
6. **Without NightCoop**, with a server in manual mode (`vc-server --echo`): you hear yourself a moment later;
   without a server: `[info] Session: NightCoop not found`.
7. **Quit the game** from the menu: no crash, and NightCoop's log doesn't report the mod as still registered.

## Alone, with the test voice

All in CET > VoiceChat > Environment. Use headphones: with speakers the recording picks up the game too.
The test voice goes through everything another player's voice does once it arrives (buffer, walls, distance,
reverb, shouting, noise, simulated network); only the real network between two games is left out.

First: **Record**, talk normally for 4-5 s, then shout for 3-4 s, **Stop**. The recording loops with a 1 s
pause. In the Environment tab's "Diagnostics" section, the line named `test` tells how the acoustics hear it
(`direct`, `thin wall`, `+ 1 glass panes`, …): write it down at every test.

| # | Where | What to do | What you should hear |
|---|---|---|---|
| V1 | anywhere, outdoors | Place it here, stay at 1 m. Network Perfect, then Typical NightCoop, then Bad | Typical: no glitches. Bad: some defects, but understandable |
| V2 | long straight street, little traffic | Place it, walk away to 3, 6, 12, 24, 48, 72 m (the distance is shown next to it) | Full up to 3 m, then half the volume at every doubling; gone around 72 m (the shouted part goes further, strong and without glitches) |
| V3 | Tom's Diner (Watson): place it at the back of the diner | Walk out of the door slowly, then move along the shop window from one end to the other | Walking out it fades and darkens gradually, without jumps. Behind the glass, at normal speech, almost nothing. Crossing from one side of the door to the other, the direction glides (no jumps). Take a screenshot of the "On the straight line" window while looking at the shop window |
| V4 | V's apartment (H10) | Place it in the bedroom, go out into the corridor, close the door | Door open: muffled, from the door. Closed: much weaker and darker, still from the door; the voice's line says "+ 1 closed doors" |
| V5 | Badlands, away from roads | Record 4-5 s of normal speech and then the shout (normal speech is needed as the reference). Place it, walk away to 50, 100, 200, 300 m | The small window says "Carries up to … m" and by how many dB the shout is over the usual. The normal part is gone within that distance; the shout carries much further, weak and "thin", without glitches. Write down "Listening to the game" (Diagnostics) with the Badlands music |
| V6 | empty underground car park, road tunnel, pedestrian tunnel | Place it at 5 m | Car park and road tunnel: clear reverb, which stays as you walk away (the voice drops, the reverb much less) and carries further than outdoors. Pedestrian tunnel: barely noticeable. Write down "Reverb used here" |
| V7 | V's apartment, Afterlife or Lizzie's, Kabuki market | Place it at 5 m | No audible reverb |
| V8 | Totentanz or Afterlife with the music, then outside in a quiet street | Place it at 10 m in both places | In the club it carries much less far than outside |
| V9 | anywhere | Tick "In my party", walk away beyond 30 m and come back | Switches to the radio (centred, phone band) around 18 m and back to the air around 15 m: standing still it must not flip back and forth |
| V10 | next to a car | Place it outside, get in and close the door | Muffled and darker |
| V11 | anywhere | Turn on the pocket radio; then open the pause menu and the hub menu (inventory, map); then call a contact | "Paused: sounds only you hear (…)" appears; the "Listening to the game" values stay still. Take a screenshot of the "Personal sound checks" line during the call |
| V12 | outdoors, quiet place | Place it, walk around it at 3 m; then stand still and turn the view 360° | The voice stays still in the world: right, left, in front, and darker behind you. No jumps while turning |
| V13 | Megabuilding H10 atrium (walkways) | Place it on a walkway, go down 1-2 floors below it; then go above it. Standing still, look up and down | From below you can tell it comes from above, from above that it comes from below (headphones help a lot). Looking straight at it, it's "in front" again. From below, take a step that puts you behind the stair parapet: the direction moves a little, over the parapet, without jumps. In front, at the same height, the timbre must not sound metallic or "in a tube" |
| V14 | anywhere, indoors or outdoors | In Diagnostics look at "Your ears". Standing, then crouching, then seated in a car. Then place the voice while crouching, stand up and stay 1 m from it | Sensible values with "(from the view)": about 1.6-1.7 m standing, about 1 m crouching. If it says "estimated", take a screenshot of the line. The voice placed while crouching comes a little from below |

## Acoustics with Steam Audio

In CET > VoiceChat > Environment, "Realistic acoustics (Steam Audio)" is on. In Diagnostics the line
"Acoustics:" says how many rays were cast, how many µs each, how long the last run took and the game
materials met. The test voice's line starts with "Steam Audio:" and says how much of the speaker is in sight,
how much goes through what hides it, and the dB of the direct part and of the rebounds.

| # | Where | What to do | What you should hear |
|---|---|---|---|
| S1 | anywhere | Start the game, load, open Diagnostics | In the log `acoustics: Steam Audio ready`; the "Acoustics:" line counts rays; no obvious FPS drop (write down the FPS with and without the tick) |
| S2 | Tom's Diner | Test voice at the back of the diner; go out and stand in front of the shop window, then in front of the door, then along the window | In front of the window much quieter and darker than in front of the door; along the window it changes slowly, without jumps. Screenshot of the small window at the three spots |
| S3 | V's apartment | Voice in the bedroom, go out, close the door; stand still, then move in front of the walls at the sides and in front of the door | Door closed: muffled, doesn't vanish; at the sides no louder than in front of the door; no movement with the door closed while standing still |
| S4 | tunnel or underground car park | Voice inside; go outdoors and turn towards the entrance | The reverb comes out of the entrance and is heard from there |
| S5 | outdoors | Walk around it, then up and down a staircase | Clear direction, front/back and above/below too |
| S6 | anywhere | Untick Steam Audio | Back to the previous model, the voice's line no longer says "Steam Audio" |

## Alone, in place of the second player

Two ways to hear yourself as another player would, both in CET > VoiceChat > Environment:

- **NightCoop echo:** host a session alone and tick "Echo through NightCoop". What you say goes the whole way
  another player's voice does (packets, encryption, checks, buffer, walls, distance), except NightCoop's
  network, and comes back from the spot where you first spoke after ticking it. In the Players list it shows
  as `echo`.
- **Server echo:** with a `vc-server --echo` server on the same PC and the mod in manual mode: the voice
  really goes over the network (UDP), then comes back from the spot where you first spoke.

Use headphones and push-to-talk: you hear your own voice a moment later, that's expected.

| # | How | What to do | What you should hear |
|---|---|---|---|
| E1 | NightCoop echo | Host alone, tick the echo, talk | You hear yourself; in Diagnostics the "Voice:" line counts the frames received, "lost in the network" 0 |
| E2 | NightCoop echo | Say a sentence, then walk away talking up to 90 m | It drops like the test voice and is gone around 72 m; shouting carries further |
| E3 | NightCoop echo | Talk in a room, go out and close the door; then turn around; then crouch at the first spot and stand 1 m away | Muffled behind walls and doors, still in the world while turning, a little from below if you "were" crouching |
| E4 | NightCoop echo | Talk in a tunnel, go outdoors and talk again | The tunnel's reverb stays on the voice coming from there |
| E5 | NightCoop echo | Join a party (General), walk away beyond 30 m and talk. Leave the party | In the party it switches to the radio even far away; out of the party, beyond 72 m, nothing |
| E6 | NightCoop echo | Talk on and off for 2-3 minutes, copy the "Voice:" line; read it again after 10 minutes | No glitches, "lost in the network" 0, "0 times at NightCoop's limit", steady latency |
| E7 | NightCoop echo | Hold the key and talk during a fast travel and while loading a save | After loading the voice resumes at once, with no piled-up delay or old pieces; "game stalls" goes up |
| E8 | NightCoop echo | Leave the session and host another one | The chat turns itself back on; the echo starts again from where you talk next |
| E9 | NightCoop echo | Set NightCoop's `sdk_session` to off and host | Clear warning "the host turned mods off for this session", no errors; turned back on, the voice returns |
| E10 | Server echo | In the project folder: `cargo run --release -p vc-server -- --echo` (the first run creates `vc-server.key`, ignored by git). In General untick "Follow the NightCoop session" and as server type `127.0.0.1:47900#FINGERPRINT` with the fingerprint ("key fingerprint") the server prints at start; talk, walk away. Then tick it again | You hear yourself from the spot where you talked; "Voice:" with a latency of a few ms; in the server log `echo anchored at` |

After every test: ok/ko and what you heard.

### Only with another player

These can't be done alone. The same rules are already checked by the automated tests, with a simulated
NightCoop with several players, slow network, loss and loading. Only NightCoop's real part is left:

- the other player's body position (if you hear each other loud at any distance, look for
  `entity_position` in the log);
- the other player showing as "1 with VoiceChat", and "(no VoiceChat)" without the mod;
- the real latency between two PCs (40-120 ms typical) and two guests hearing each other through the host.

To send: the `voicechat-*.log` logs and NightCoop's, the "Voice:" lines from E6, ok/ko per item.
