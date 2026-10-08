ARG BASE_IMAGE
FROM ${BASE_IMAGE}
USER root
COPY packages.txt rust-toolchain.toml /opt/tessera-ci/
RUN apt-get update -qq \
    && xargs -r apt-get install -y -qq --no-install-recommends < /opt/tessera-ci/packages.txt \
    && rm -rf /var/lib/apt/lists/*
# The build script reads and validates this value from rust-toolchain.toml.
ARG RUST_VERSION
RUN curl --fail --silent --show-error --location https://sh.rustup.rs -o /opt/tessera-ci/rustup-init.sh \
    && sh /opt/tessera-ci/rustup-init.sh -y --profile minimal --default-toolchain "$RUST_VERSION" --component rustfmt,clippy \
    && rm /opt/tessera-ci/rustup-init.sh
ENV PATH="/root/.cargo/bin:${PATH}"
ARG SOURCE_REVISION
ARG BASE_IMAGE
LABEL org.opencontainers.image.source="https://git.oklabs.uk/BeFeast/tessera" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.base.name="${BASE_IMAGE}"
RUN rustc -vV > /opt/tessera-ci/rustc.txt \
    && cargo -V > /opt/tessera-ci/cargo.txt \
    && cargo clippy -V > /opt/tessera-ci/clippy.txt \
    && rustfmt -V > /opt/tessera-ci/rustfmt.txt \
    && dpkg-query -W > /opt/tessera-ci/packages-installed.txt \
    && node --version && python3 --version && git --version && mold --version
