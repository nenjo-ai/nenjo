# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.94.0
ARG DEBIAN_VERSION=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder

WORKDIR /src

ENV CARGO_TERM_COLOR=always

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
        --package nenjo-cli \
        --package nenpm-cli \
        --package nenjoup-cli \
    && mkdir -p /out \
    && cp target/release/nenjo /out/nenjo \
    && cp target/release/nenpm /out/nenpm \
    && cp target/release/nenjoup /out/nenjoup

FROM debian:${DEBIAN_VERSION}-slim AS runtime

ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED=unknown

LABEL org.opencontainers.image.title="Nenjo Worker" \
      org.opencontainers.image.description="Production Nenjo platform worker image" \
      org.opencontainers.image.source="https://github.com/nenjo-ai/nenjo" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}" \
      org.opencontainers.image.licenses="Apache-2.0"

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        coreutils \
        curl \
        dash \
        findutils \
        gawk \
        git \
        git-lfs \
        grep \
        gzip \
        jq \
        openssh-client \
        python3 \
        python3-pip \
        python3-venv \
        ripgrep \
        sed \
        tar \
        tzdata \
        unzip \
        wget \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /out/nenjo /usr/local/bin/nenjo
COPY --from=builder /out/nenpm /usr/local/bin/nenpm
COPY --from=builder /out/nenjoup /usr/local/bin/nenjoup

RUN useradd --create-home --home-dir /home/nenjo --shell /bin/bash --uid 10001 nenjo \
    && mkdir -p /home/nenjo/.nenjo/workspace \
    && chown -R nenjo:nenjo /home/nenjo

ENV HOME=/home/nenjo \
    NENJO_DIR=/home/nenjo/.nenjo \
    NENJO_NO_UPDATE_CHECK=1 \
    PATH=/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin

USER nenjo

RUN git lfs install --skip-repo

WORKDIR /home/nenjo/.nenjo/workspace

ENTRYPOINT ["nenjo"]
CMD ["run"]

FROM runtime AS dev

USER root

LABEL org.opencontainers.image.title="Nenjo Worker Dev" \
      org.opencontainers.image.description="Developer toolbox image for the Nenjo platform worker"

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/home/nenjo/.cargo \
    PATH=/usr/local/cargo/bin:/home/nenjo/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        clang \
        cmake \
        docker.io \
        file \
        gh \
        less \
        make \
        nano \
        netcat-openbsd \
        nodejs \
        npm \
        pipx \
        pkg-config \
        procps \
        vim-tiny \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder --chown=nenjo:nenjo /usr/local/cargo /usr/local/cargo
COPY --from=builder --chown=nenjo:nenjo /usr/local/rustup /usr/local/rustup

RUN mkdir -p /home/nenjo/.cargo \
    && chown -R nenjo:nenjo /home/nenjo/.cargo \
    && if command -v corepack >/dev/null 2>&1; then corepack enable; fi

USER nenjo
