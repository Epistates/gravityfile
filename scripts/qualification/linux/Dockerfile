FROM rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 bash zsh fish \
    && apt-get clean \
    && useradd --create-home --uid 10001 qualification \
    && mkdir -p /build-target /home/qualification/.cargo \
    && chown -R 10001:10001 /build-target /home/qualification/.cargo

ENV CARGO_HOME=/home/qualification/.cargo
ENV CARGO_TARGET_DIR=/build-target
USER 10001:10001
WORKDIR /workspace
