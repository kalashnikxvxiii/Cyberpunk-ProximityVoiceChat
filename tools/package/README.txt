VoiceChat @VERSION@ - proximity voice chat for NightCoop
=========================================================

WHAT YOU NEED
- Cyberpunk 2077 with NightCoop (RED4ext comes with NightCoop).
- Cyber Engine Tweaks (CET): menu, guided setup, HUD icon, walls and
  reverb. Without CET the voice works anyway (talk key: mouse 4).
- Every player in the session needs VoiceChat, same version.

INSTALL
1. Close the game.
2. Extract the zip into the game folder (the one with "bin" and "red4ext").
   When updating, overwrite freely: your settings are in
   red4ext\plugins\VoiceChat\config.ini and are left alone.
3. Start the game and open the CET overlay: the guided setup starts
   (microphone, headphones, talk key, language). One minute and you're done.

USE
- Host or join a NightCoop co-op session: the voice chat connects by
  itself, through NightCoop. No addresses to exchange, no ports to open.
- In the co-op lobby everybody hears everybody. In the world you hear who
  is near you, from where they are; thick walls block voices, doors and
  openings let them through muffled.
- Talk key: mouse 4 (the side mouse button) or the one you choose.
- Party: same party name = you hear each other at any distance (like a
  radio). CET menu > VoiceChat > Party.

IF YOU CAN'T HEAR EACH OTHER
CET menu > VoiceChat > Diagnostics: at the top who has the mod and how the
voice is doing (latency, loss), below a table with what happened and what to
do. Most common causes:
- NightCoop too old, or its SDK for mods switched off: update NightCoop
  and keep the SDK on in its settings.
- The host turned mods off for the session.
- Different mod versions.
- The push-to-talk key is also a NightCoop key: pick another one.

YOUR OWN SERVER (OPTIONAL, WITHOUT NIGHTCOOP)
Manual mode (CET menu > VoiceChat > General) connects to a vc-server
(VoiceChat-relay zip) that anyone with a reachable PC or server can run.

SECURITY AND PRIVACY
- In a NightCoop session the mod has no network code of its own: voice
  goes through NightCoop's encrypted links, sealed again with a key the
  host draws for each session. It opens no ports and shows your IP address
  to nobody. The host, as with every co-op mod, can see the mod's data.
- Only players in the same NightCoop session who have the mod hear you.

LOG FILE
<game>\red4ext\logs\voicechat-*.log
