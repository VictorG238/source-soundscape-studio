FROM rust:1.100.0-slim-trixie AS base
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libx11-dev libxi-dev libxkbcommon-dev libgl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy
WORKDIR /project
ENV CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=/build

FROM base AS development
RUN apt-get update && apt-get install -y --no-install-recommends libasound2-dev && rm -rf /var/lib/apt/lists/*

FROM base AS windows
RUN apt-get update && apt-get install -y --no-install-recommends clang lld llvm cmake liblzma-dev \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-pc-windows-msvc && cargo install --locked cargo-xwin --version 0.23.1
ENV XWIN_ARCH=x86_64 XWIN_CACHE_DIR=/sdk
