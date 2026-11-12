FROM debian:bookworm-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"
# Install from the same pin as local/CI builds before copying application sources.
WORKDIR /source/inbox
COPY inbox/rust-toolchain.toml rust-toolchain.toml
RUN set -eu; \
    case "$(dpkg --print-architecture)" in \
        amd64) rustup_target=x86_64-unknown-linux-gnu ;; \
        arm64) rustup_target=aarch64-unknown-linux-gnu ;; \
        *) echo "Unsupported Rust build architecture" >&2; exit 1 ;; \
    esac; \
    rustup_url="https://static.rust-lang.org/rustup/dist/$rustup_target/rustup-init"; \
    curl --fail --silent --show-error --location "$rustup_url" -o rustup-init; \
    curl --fail --silent --show-error --location "$rustup_url.sha256" -o rustup-init.sha256; \
    sha256sum --check rustup-init.sha256; \
    chmod +x rustup-init; \
    ./rustup-init -y --no-modify-path --profile minimal --default-toolchain none; \
    rustc --version; \
    rm rustup-init rustup-init.sha256
WORKDIR /source
COPY inbox inbox
COPY crates/okilum-sync crates/okilum-sync
COPY crates/okilum-network-access crates/okilum-network-access
COPY crates/okilum-write-authority crates/okilum-write-authority
COPY web/inbox web/inbox
ENV CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_DEBUG=0
RUN cd inbox && cargo build --locked --release -p okilum-inboxd
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 sqlite3 python3 curl
COPY --from=build /source/inbox/target/release/okilum-inboxd /usr/local/bin/okilum-inboxd
COPY inbox/deploy/entrypoint.sh /usr/local/bin/inbox-entrypoint
COPY inbox/deploy/backup.py /usr/local/bin/inbox-backup
COPY inbox/forgejo/collector.py /usr/local/bin/forgejo-collector
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/inbox-entrypoint"]
