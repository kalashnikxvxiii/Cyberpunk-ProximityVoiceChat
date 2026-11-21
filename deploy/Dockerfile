# Build from the repo root: docker compose -f deploy/compose.yaml up -d --build
FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/protocol ./crates/protocol
COPY crates/server ./crates/server
COPY crates/rendezvous ./crates/rendezvous
# Only the server's crates (the rendezvous one is for its tests): the plugin
# is Windows-only and its DSP isn't needed.
RUN sed -i -e 's|^members = .*|members = ["crates/protocol", "crates/server", "crates/rendezvous"]|' \
        -e 's|^default-members = .*|default-members = ["crates/protocol", "crates/server"]|' Cargo.toml \
    && cargo build --release -p vc-server

FROM debian:stable-slim
COPY --from=build /src/target/release/vc-server /usr/local/bin/vc-server
# The server key lives here; players pin its fingerprint, so keep the volume.
RUN mkdir /data && chown nobody /data
VOLUME /data
USER nobody
ENTRYPOINT ["/usr/local/bin/vc-server"]
