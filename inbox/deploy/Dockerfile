FROM rust:1.96.1-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev
WORKDIR /source
COPY inbox inbox
COPY crates/tessera-sync crates/tessera-sync
COPY web/inbox web/inbox
ENV CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_DEBUG=0
RUN cargo build --manifest-path inbox/Cargo.toml --locked --release -p tessera-inboxd
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 sqlite3 python3 curl
COPY --from=build /source/inbox/target/release/tessera-inboxd /usr/local/bin/tessera-inboxd
COPY inbox/deploy/entrypoint.sh /usr/local/bin/inbox-entrypoint
COPY inbox/deploy/backup.py /usr/local/bin/inbox-backup
COPY inbox/forgejo/collector.py /usr/local/bin/forgejo-collector
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/inbox-entrypoint"]
