# Voice Chat per NightCoop — architettura (bozza)

Obiettivo: una voice chat in stile *Simple Voice Chat* (Minecraft) per Cyberpunk 2077,
pensata per le sessioni NightCoop: voce di prossimità + party (gruppi) che si sentono
a qualsiasi distanza.

## Vincoli emersi dalla ricerca

- **NightCoop è closed source e non espone API** (niente funzioni esportate, classi RTTI
  o hook per altre mod). Pubblico è solo l'hub (`github.com/Twenfys/nightcoop-hub`, C++ + ENet + Monocypher).
  → La voice chat deve essere **indipendente**: propria rete, proprio server.
- NightCoop è un plugin RED4ext puro: **non richiede CET né redscript**. Per restare in linea,
  il nucleo della mod deve funzionare con solo RED4ext.
- Versioni gioco supportate da NightCoop: 2.50 e 2.31 (Windows; su Linux via Proton).
- Unica interop "gratuita": `red4ext/plugins/NightCoop/name.txt` contiene il nickname del giocatore
  → lo usiamo come nome di default (sola lettura).

## Idea chiave: ogni client manda la propria posizione

Non serve sapere dove NightCoop mette gli altri giocatori. Ogni client:
1. legge posizione di V e orientamento della camera dal gioco (locale, sempre disponibile);
2. allega la posizione a ogni pacchetto voce;
3. chi riceve calcola volume (attenuazione per distanza) e panning stereo rispetto al proprio ascoltatore.

Siccome NightCoop sincronizza il mondo, le coordinate di tutti sono nello stesso spazio.

## Componenti

```
┌──────────── gioco (Windows / Proton) ─────────────┐
│ VoiceChat.dll (plugin RED4ext, Rust)              │
│  ├─ game:   posizione player + camera (red4ext-rs)│
│  ├─ audio:  cattura mic + playback (cpal/WASAPI)  │
│  ├─ codec:  Opus 48 kHz mono, frame 20 ms         │
│  ├─ mix:    attenuazione + pan per ogni speaker   │
│  ├─ net:    client UDP (protocollo nostro)        │
│  └─ api:    funzioni globali RTTI (per UI)        │
│ UI opzionale: mod CET (ImGui) che chiama l'api    │
└───────────────────────┬───────────────────────────┘
                        │ UDP
              ┌─────────┴──────────┐
              │ vc-server (Rust)   │  relay leggero, self-hostable (Docker)
              │ stanze, party,     │  inoltra voce solo a chi è
              │ filtro distanza    │  in raggio o nello stesso party
              └────────────────────┘
```

### Perché un server relay (come Simple Voice Chat) e non P2P
- NAT: NightCoop ha dovuto scrivere un hub per bucare i router; un relay unico evita il problema.
- Banda: per 4–8 giocatori, Opus a ~24 kbps → trascurabile anche in relay.
- Il server può fare il filtro di distanza e la logica dei party in un posto solo.
- Più avanti: modalità "server integrato" nell'host (stesso codice, avviato dal plugin).

## Funzionalità (MVP → dopo)

MVP
- Connessione a server + codice stanza; nickname da NightCoop.
- Push-to-talk (tasto configurabile) e modalità a soglia (voice activation).
- Prossimità: raggio max configurabile (default 48 m), attenuazione lineare/log, pan stereo.
- Party: crea / entra / esci; i membri si sentono ovunque, in "radio" (senza attenuazione).
- Volume per giocatore, mute.
- Config in `red4ext/plugins/VoiceChat/config.ini`.

Dopo
- UI in gioco (CET/ImGui o nativeSettings), icona "sta parlando".
- Party con password, sussurro (raggio ridotto), effetto "radio" per i party.
- Occlusione/riverbero semplice, HRTF.
- Crittografia (X25519 + XChaCha20-Poly1305, come NightCoop; crate `x25519-dalek`, `chacha20poly1305`).

## Stack e toolchain

- **Rust** (workspace Cargo), stesso codice di protocollo per plugin e server.
- Plugin: `red4ext-rs` (wrapper di RED4ext.SDK: bindgen + piccolo bridge C++ via CMake),
  `cpal` (audio), `audiopus` (Opus, libopus compilata staticamente), `std::net::UdpSocket`.
- Server: `tokio`, nativo Linux, distribuibile via Docker.
- Sviluppo su Linux: target `x86_64-pc-windows-msvc` con **rustup + cargo-xwin**
  (clang-cl/lld-link + CRT e Windows SDK scaricati da xwin).
  Il README di red4ext-rs dice che la cross-compilazione da Linux non è supportata:
  è il primo rischio da verificare. Piano B: build su GitHub Actions (runner Windows).
- Test: gioco GOG via Heroic + Proton; RED4ext già installato.

## Struttura repo prevista

```
crates/protocol/   pacchetti e serializzazione (condiviso)
crates/plugin/     VoiceChat.dll (cdylib, RED4ext)
crates/server/     vc-server
deploy/            Dockerfile + compose per il server
tools/             build, installazione, pacchetto zip
cet/               UI opzionale per Cyber Engine Tweaks (da fare)
docs/
```

## Punti da verificare (rischi)

1. ~~red4ext-rs compila da Linux con cargo-xwin~~ ✅ sì, con `tools/build-plugin.sh` (overlay per 4 include con maiuscole sbagliate nell'SDK). Caricato in gioco (2.50, Proton) insieme a NightCoop: OK.
2. ~~Lettura posizione via RTTI senza redscript, tick affidabile~~ ✅ `GetPlayer` + `GetWorldPosition`/`GetWorldForward` dal listener `Running` (ogni frame). Coordinate in metri, Z verso l'alto, `fwd` orizzontale normalizzato. Camera vera: da fare (veicoli/terza persona).
3. ~~Cattura microfono sotto Proton~~ ✅ eco locale via Opus nitida, nessun impatto sul gioco (PipeWire 48 kHz stereo F32). Note: picchi > 1.0 in ingresso → serve limiter/soft clip; bitrate Opus auto ~45–50 kbps → fissare ~24 kbps.
4. Convivenza con NightCoop (nessun conflitto di hook; porte UDP diverse da 47800/47810+).
