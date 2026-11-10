#!/usr/bin/env bash
# Local CI: runs the same checks as .github/workflows/ci.yml on this machine
# (GitHub Actions minutes are used up). Exit code 0 only if every required job
# passed. Run from any directory of a checkout or worktree:
#
#   just ci                 # all jobs
#   just ci rust web        # only the named jobs
#
# Jobs: rust, web, audit, migrations, rollback.
#   rust        fmt check, clippy (-D warnings, --locked), tests (--locked)
#   web         API types up to date, format, lint, typecheck, tests, build
#   audit       `just audit`: cargo-deny, cargo-audit, pnpm audit --prod
#   migrations  committed migrations unchanged against the merge base with
#               origin/main (CI's migrations-immutable)
#   rollback    ADR 0008 rollback check (scripts/rollback-check.sh); needs a
#               running docker daemon and `docker login ghcr.io`. Without a
#               daemon it is reported as SKIPPED and does not fail the run,
#               unless CI_REQUIRE_ROLLBACK=1.
#
# Writes a summary to target/ci-local/summary.txt and one log per job next to
# it, so a PR description can quote the result.

set -uo pipefail

# The checkout to test: the git work tree of the current directory, else the
# one this script lives in (so a copy of the script can test another worktree).
repo=$(git rev-parse --show-toplevel 2>/dev/null || (cd "$(dirname "$0")/.." && pwd))
cd "$repo"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR=never
export RUSTFLAGS="-D warnings"

out="$repo/target/ci-local"
mkdir -p "$out"
summary="$out/summary.txt"
: >"$summary"

jobs=("$@")
[ ${#jobs[@]} -eq 0 ] && jobs=(rust web audit migrations rollback)

status=0
record() { # job result seconds
  printf '%-11s %-8s %4ss\n' "$1" "$2" "$3" | tee -a "$summary"
}

run_job() { # name, then the function to run
  local name=$1 fn=$2 start rc
  start=$(date +%s)
  echo "==> $name"
  if "$fn" >"$out/$name.log" 2>&1; then rc=0; else rc=$?; fi
  local secs=$(( $(date +%s) - start ))
  if [ "$rc" -eq 0 ]; then
    record "$name" PASS "$secs"
  elif [ "$rc" -eq 77 ]; then
    record "$name" SKIPPED "$secs"
    sed -n '1,5p' "$out/$name.log" | sed 's/^/    /'
  else
    record "$name" FAIL "$secs"
    tail -n 40 "$out/$name.log" | sed 's/^/    /'
    status=1
  fi
}

job_rust() {
  cargo fmt --all --check &&
    cargo clippy --workspace --all-targets --locked -- -D warnings &&
    cargo test --workspace --locked &&
    cargo build -p starr-server --locked
}

job_web() {
  (cd web && pnpm install --frozen-lockfile) &&
    mkdir -p target &&
    cargo run -q -p starr-server -- openapi >target/openapi.json &&
    (cd web && pnpm gen-api && pnpm exec prettier --write src/api/schema.d.ts) &&
    git diff --exit-code -- web/src/api/schema.d.ts &&
    (cd web && pnpm format:check && pnpm lint && pnpm typecheck && pnpm test && pnpm build)
}

job_audit() {
  cargo deny check &&
    cargo audit --deny warnings &&
    (cd web && pnpm audit --prod)
}

job_migrations() {
  git fetch -q origin main || return 1
  local base changed
  base=$(git merge-base origin/main HEAD) || return 1
  changed=$(git diff --name-only --diff-filter=MDR "$base" -- crates/starr-db/migrations/)
  if [ -n "$changed" ]; then
    echo "Modified existing migrations:"
    echo "$changed"
    return 1
  fi
  echo "no committed migration changed since $base"
}

job_rollback() {
  if ! docker info >/dev/null 2>&1; then
    echo "docker daemon not running: rollback check not executed"
    [ "${CI_REQUIRE_ROLLBACK:-0}" = 1 ] && return 1
    return 77
  fi
  [ -x target/debug/starr ] || cargo build -p starr-server --locked || return 1
  STARR_BIN="$repo/target/debug/starr" scripts/rollback-check.sh
}

for job in "${jobs[@]}"; do
  case "$job" in
    rust) run_job rust job_rust ;;
    web) run_job web job_web ;;
    audit) run_job audit job_audit ;;
    migrations) run_job migrations job_migrations ;;
    rollback) run_job rollback job_rollback ;;
    *) echo "unknown job: $job (rust, web, audit, migrations, rollback)"; exit 2 ;;
  esac
done

echo "head $(git rev-parse --short HEAD) · $(date -u +%Y-%m-%dT%H:%MZ)" | tee -a "$summary"
[ "$status" -eq 0 ] && echo "LOCAL CI: PASS" | tee -a "$summary" || echo "LOCAL CI: FAIL" | tee -a "$summary"
exit "$status"
