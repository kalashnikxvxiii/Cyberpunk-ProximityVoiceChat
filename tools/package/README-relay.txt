VoiceChat @VERSION@ - relay server (optional)
==============================================

Only needed when guests can't connect to the host directly.
Run it on a PC or server reachable from the internet.

START
  Windows:  vc-server.exe
  Linux:    ./vc-server
Options: --bind 0.0.0.0:47900   --range 48   --key vc-server.key
On first start it creates its key (vc-server.key, keep it) and prints the
fingerprint, for example:  key fingerprint K3QF-9X2M-TV7A-0D4R

NETWORK
Open UDP port 47900 (or the one you chose) on the router and firewall.

IN THE GAME
Everybody turns off "Follow the NightCoop session" in CET menu >
VoiceChat > General (manual mode) and enters the same room code and the
server:
  address:47900#FINGERPRINT     (e.g. 203.0.113.7:47900#K3QF-9X2M-TV7A-0D4R)
In a NightCoop session you don't need it: voice goes through NightCoop.

The server routes the voice but can't listen to it: it is encrypted from
player to player, with a key that comes from the room code. Use "New random
code" next to the room code and share it privately: a short code could be
guessed by whoever runs the server.
