# Voice Chat (Cyberpunk 2077 · NightCoop)

Voice chat di prossimità con party per le sessioni co-op di [NightCoop](https://nightcoop.net/),
ispirata a *Simple Voice Chat* di Minecraft. Plugin RED4ext in Rust + server relay leggero.

Stato: progettazione. Vedi [docs/ARCHITETTURA.md](docs/ARCHITETTURA.md).

## Build (Linux → Windows)

Requisiti: `rustup` con target `x86_64-pc-windows-msvc`, `cargo-xwin`, clang/lld, cmake, ninja.

```sh
tools/build-plugin.sh   # → target/x86_64-pc-windows-msvc/release/VoiceChat.dll
tools/install.sh        # copia in <gioco>/red4ext/plugins/VoiceChat/ (CP2077_DIR per cambiare percorso)
```

## Server

```sh
cargo run --release -p vc-server -- --bind 0.0.0.0:47900 --range 48 [--echo]
docker compose -f deploy/compose.yaml up -d --build   # oppure in Docker, rete host
```

`--echo` rimanda a ogni giocatore la propria voce dal punto in cui si trovava entrando: serve per i test da soli.
Il server va raggiunto su UDP 47900 (port forwarding sul router).

## Pacchetto per gli altri giocatori

```sh
tools/package.sh 203.0.113.7:47900 [STANZA]   # → dist/VoiceChat-<versione>.zip + dist/config.ini
```

Senza `STANZA` genera un codice casuale. `tools/install.sh` usa `dist/config.ini` alla prima installazione.
