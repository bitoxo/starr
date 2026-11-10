# syntax=docker/dockerfile:1

# --- web UI ---
FROM node:24-alpine AS web
WORKDIR /src/web
RUN corepack enable
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# --- server ---
# cargo-chef splits the build: dependencies compile in their own layer, keyed
# only on the dependency graph, so the CI layer cache reuses them across
# releases and only Starr's own crates rebuild.
FROM rust:1-slim-bookworm AS chef
WORKDIR /src
# Resolve rust-toolchain.toml before cooking: otherwise the cook uses the
# image's compiler and the later build auto-installs `stable`, which would
# throw the cooked dependencies away.
COPY rust-toolchain.toml ./
RUN cargo install cargo-chef --version 0.1.78 --locked

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS server
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p starr-server --recipe-path recipe.json
COPY . .
COPY --from=web /src/web/dist ./web/dist
RUN cargo build --release --locked -p starr-server \
    && cp target/release/starr /usr/local/bin/starr

# --- runtime ---
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --user-group --no-create-home starr \
    && mkdir /config && chown starr:starr /config
COPY --from=server /usr/local/bin/starr /usr/local/bin/starr

ENV STARR_DATA_DIR=/config \
    STARR_BIND=0.0.0.0 \
    STARR_PORT=7979
VOLUME /config
EXPOSE 7979
USER starr
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD curl -fsS http://localhost:7979/api/v1/health || exit 1
ENTRYPOINT ["/usr/local/bin/starr"]
