ARG BASE_IMAGE
FROM ${BASE_IMAGE}
USER root
COPY packages.txt rust-toolchain.toml /opt/okilum-ci/
RUN apt-get update -qq \
    && xargs -r apt-get install -y -qq --no-install-recommends < /opt/okilum-ci/packages.txt \
    && rm -rf /var/lib/apt/lists/*
# The build script reads and validates this value from rust-toolchain.toml.
ARG RUST_VERSION
RUN curl --fail --silent --show-error --location https://sh.rustup.rs -o /opt/okilum-ci/rustup-init.sh \
    && sh /opt/okilum-ci/rustup-init.sh -y --profile minimal --default-toolchain "$RUST_VERSION" --component rustfmt,clippy \
    && rm /opt/okilum-ci/rustup-init.sh
ENV PATH="/root/.cargo/bin:${PATH}"
ARG SOURCE_REVISION
ARG BASE_IMAGE
LABEL org.opencontainers.image.source="https://git.oklabs.uk/BeFeast/okilum" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.base.name="${BASE_IMAGE}"
RUN rustc -vV > /opt/okilum-ci/rustc.txt \
    && cargo -V > /opt/okilum-ci/cargo.txt \
    && cargo clippy -V > /opt/okilum-ci/clippy.txt \
    && rustfmt -V > /opt/okilum-ci/rustfmt.txt \
    && dpkg-query -W > /opt/okilum-ci/packages-installed.txt \
    && node --version && python3 --version && git --version && mold --version
