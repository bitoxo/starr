# Starr task runner. `just` lists all recipes. These are the canonical commands
# for humans, Claude and CI alike.

set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Everything CI checks. Run before every commit/PR.
check: fmt-check lint test web-check

# Local CI: the jobs of .github/workflows/ci.yml on this machine (GitHub
# Actions minutes are used up). `just ci` runs all, `just ci rust web` some.
# Summary and logs in target/ci-local/.
ci *JOBS:
    scripts/ci-local.sh {{JOBS}}

# Format Rust and web code.
fmt:
    cargo fmt --all
    cd web && pnpm format

fmt-check:
    cargo fmt --all --check
    cd web && pnpm format:check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# Fast compile feedback (no codegen).
c:
    cargo check --workspace --all-targets

# Regenerate web/src/api/schema.d.ts from the Rust OpenAPI spec.
gen-api:
    mkdir -p target
    cargo run -q -p starr-server -- openapi > target/openapi.json
    cd web && pnpm gen-api && pnpm exec prettier --write src/api/schema.d.ts

# Fails if the committed TS API types are out of date.
gen-api-check: gen-api
    git diff --exit-code -- web/src/api/schema.d.ts

# Dependency advisories and licences: cargo-deny (deny.toml), cargo-audit
# (.cargo/audit.toml) and pnpm audit of the web production dependencies.
# Needs `cargo install cargo-deny cargo-audit`.
audit:
    cargo deny check
    cargo audit --deny warnings
    cd web && pnpm audit --prod

web-install:
    cd web && pnpm install --frozen-lockfile

web-check:
    cd web && pnpm lint && pnpm typecheck && pnpm test

web-build:
    cd web && pnpm build

# Run the server (API + embedded UI) with local data in ./data.
run *ARGS:
    STARR_LOG=${STARR_LOG:-info,starr=debug} cargo run -p starr-server -- {{ARGS}}

# Dev mode: Rust server on :7979 + Vite with HMR on :5173 (open :5173).
# STARR_PUBLIC_URL defaults to the Vite origin so login and CSRF checks work.
dev:
    trap 'kill 0' EXIT; STARR_PUBLIC_URL=${STARR_PUBLIC_URL:-http://localhost:5173} just run & (cd web && pnpm dev) & wait

# New SQLx migration: just migration add_users
migration NAME:
    cd crates/starr-db && sqlx migrate add {{NAME}}

# Release build with embedded UI.
build: web-build
    cargo build --release -p starr-server

# ADR 0008: previous release image still serves login on the current schema.
# Needs docker and `docker login ghcr.io`; uses a temp dir and port 7989.
rollback-check:
    scripts/rollback-check.sh

docker-build:
    docker build -t starr:dev .

# Regenerate docs/configuration.md from the config schema.
gen-config-docs:
    cargo run -q -p starr-server -- config-docs > docs/configuration.md
