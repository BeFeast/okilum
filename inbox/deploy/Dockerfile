FROM rust:1.99.0-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev
WORKDIR /source
COPY inbox inbox
COPY crates/okilum-sync crates/okilum-sync
COPY web/inbox web/inbox
ENV CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_DEBUG=0
RUN cargo build --manifest-path inbox/Cargo.toml --locked --release -p okilum-inboxd
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 sqlite3 python3 curl
COPY --from=build /source/inbox/target/release/okilum-inboxd /usr/local/bin/okilum-inboxd
COPY inbox/deploy/entrypoint.sh /usr/local/bin/inbox-entrypoint
COPY inbox/deploy/backup.py /usr/local/bin/inbox-backup
COPY inbox/forgejo/collector.py /usr/local/bin/forgejo-collector
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/inbox-entrypoint"]
