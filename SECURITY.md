# Security

VoiceChat carries players' voices, so security problems matter.

## Reporting a vulnerability

Please don't open a public issue. Use GitHub's private reporting instead:
**Security** tab > **Report a vulnerability** on this repository. Include the version, what an attacker
can do, and the steps to reproduce it.

You'll get an answer within a week. Fixes ship as a new release on Nexus Mods and here, and the report is
credited unless you'd rather not be named.

## Scope

- The RED4ext plugin (`crates/plugin`), the chat over NightCoop's SDK (`crates/coop`), the protocol and its
  encryption (`crates/protocol`), the relay server (`crates/server`), the CET scripts (`cet/`).
- What the design protects and what it doesn't is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#security).
  Known and accepted: the host of a NightCoop session sees the mod's data, as with every co-op mod.

Vulnerabilities in NightCoop itself go to its moderators privately (https://discord.gg/nightcoop).
