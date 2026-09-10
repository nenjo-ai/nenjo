# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.94.0
ARG DEBIAN_VERSION=bookworm
ARG NODE_VERSION=24
ARG AGENT_BROWSER_VERSION=0.32.2

FROM node:${NODE_VERSION}-${DEBIAN_VERSION}-slim AS node-runtime

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
    XDG_CACHE_HOME=/home/nenjo/.cache \
    PIP_CACHE_DIR=/home/nenjo/.cache/pip \
    VIRTUAL_ENV=/home/nenjo/.venv \
    PATH=/home/nenjo/.venv/bin:/home/nenjo/.local/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin

USER nenjo

RUN mkdir -p /home/nenjo/.local/bin "$PIP_CACHE_DIR" \
    && python3 -m venv "$VIRTUAL_ENV" \
    && git lfs install --skip-repo

WORKDIR /home/nenjo/.nenjo/workspace

ENTRYPOINT ["nenjo"]
CMD ["run"]

FROM runtime AS dev

USER root

LABEL org.opencontainers.image.title="Nenjo Worker Dev" \
      org.opencontainers.image.description="Developer toolbox image for the Nenjo platform worker"

ENV RUSTUP_HOME=/home/nenjo/.rustup \
    CARGO_HOME=/home/nenjo/.cargo \
    NPM_CONFIG_CACHE=/home/nenjo/.npm \
    NPM_CONFIG_PREFIX=/home/nenjo/.local \
    PATH=/home/nenjo/.cargo/bin:${PATH}

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
        pipx \
        pkg-config \
        procps \
        vim-tiny \
    && rm -rf /var/lib/apt/lists/*

RUN install -d -m 0755 -o nenjo -g nenjo "$CARGO_HOME"

COPY --from=builder --chown=nenjo:nenjo /usr/local/cargo/bin /home/nenjo/.cargo/bin
COPY --from=builder --chown=nenjo:nenjo /usr/local/rustup /home/nenjo/.rustup
COPY --from=node-runtime /usr/local/bin/node /usr/local/bin/node
COPY --from=node-runtime /usr/local/lib/node_modules /usr/local/lib/node_modules

RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && if command -v corepack >/dev/null 2>&1; then corepack enable; fi

USER nenjo

RUN mkdir -p "$NPM_CONFIG_CACHE" "$CARGO_HOME/registry" "$CARGO_HOME/git" \
    # Shell tools filter environment variables, so persist the npm prefix too.
    && npm config set prefix "$NPM_CONFIG_PREFIX" --location=user \
    && npm cache verify

FROM dev AS heavy

ARG AGENT_BROWSER_VERSION

USER root

LABEL org.opencontainers.image.title="Nenjo Worker Heavy" \
      org.opencontainers.image.description="Developer toolbox image with agent-browser and Chromium"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        chromium \
    && rm -rf /var/lib/apt/lists/*

USER nenjo

RUN npm install --global "agent-browser@${AGENT_BROWSER_VERSION}" \
    && npm cache clean --force \
    && agent-browser --version

ENV AGENT_BROWSER_EXECUTABLE_PATH=/usr/bin/chromium

RUN install -d -m 0700 /home/nenjo/.nenjo/browser-state \
    && ln -s /home/nenjo/.nenjo/browser-state /home/nenjo/.agent-browser

RUN npm cache verify \
    && agent-browser doctor --offline --quick --json
