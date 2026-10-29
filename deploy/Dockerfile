# Build from the repo root: docker compose -f deploy/compose.yaml up -d --build
FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/protocol ./crates/protocol
COPY crates/server ./crates/server
# The plugin is Windows-only; drop it so its dependencies aren't fetched.
RUN sed -i 's|"crates/plugin", ||' Cargo.toml && cargo build --release -p vc-server

FROM debian:stable-slim
COPY --from=build /src/target/release/vc-server /usr/local/bin/vc-server
USER nobody
ENTRYPOINT ["/usr/local/bin/vc-server"]
