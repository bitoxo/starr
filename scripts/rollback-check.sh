#!/usr/bin/env bash
# Rollback compatibility check (ADR 0008): the previous release image must
# still serve login on a database migrated by the current build.
#
# 1. Starts the current build on a fresh data dir, completes setup through the
#    API and creates an admin, a member with a password, an invitation and a
#    Lidarr (unreachable, nothing is contacted) as the music source, a
#    Navidrome (unreachable, a login credential) as its consume service, a
#    Whisparr (unreachable) as the adult source and, written straight into the
#    database after the current build stopped, adult scene requests (spec
#    0007 slice 4: one waiting, one approved with its acquisition; neutral
#    placeholders only).
# 2. Starts the previous release image on that data dir.
# 3. Fatal: container healthy, health endpoint, admin and member login,
#    /api/v1/auth/me, the category list with the music source (ADR 0013) and
#    the adult source (spec 0007 slice 3), and the request lists with the
#    adult scene requests present (spec 0007 slice 4).
#    Non-fatal: a few list endpoints, printed as a summary.
#
# Environment:
#   STARR_BIN       current binary (default: `cargo build -p starr-server`,
#                   then target/debug/starr)
#   ROLLBACK_IMAGE  image to roll back to (default: ghcr.io/bitoxo/starr:<x.y.z>
#                   of the newest v* tag before HEAD)
#   ROLLBACK_PORT   host port for both runs (default 7989)
#
# Needs docker, curl, jq and python3 (its sqlite3 module). The ghcr package is private: `docker login ghcr.io`
# first. An image this script pulled is removed again at the end.

set -euo pipefail

port=${ROLLBACK_PORT:-7989}
origin="http://localhost:$port"
base="$origin/api/v1"
admin_user=rollback-admin
admin_pass=rollback-admin-password
member_user=rollback-member
member_pass=rollback-member-password

repo=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/starr-rollback.XXXXXX")
data="$work/config"
mkdir -p "$data"
container="starr-rollback-$$"
server_pid=
pulled_image=

log() { printf '\n==> %s\n' "$*"; }
fail() {
  printf '\nFAIL: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  local status=$?
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if docker container inspect "$container" >/dev/null 2>&1; then
    if [ "$status" -ne 0 ]; then
      log "previous release log"
      docker logs "$container" 2>&1 | tail -n 50 || true
    fi
    docker rm -f "$container" >/dev/null 2>&1 || true
  fi
  if [ -n "$pulled_image" ]; then
    docker rmi "$pulled_image" >/dev/null 2>&1 || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$work/current.log" ]; then
    log "current build log"
    tail -n 50 "$work/current.log" || true
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT

# curl wrapper: prints the HTTP status, writes the body to $work/body.
# Mutating requests carry the Origin header the CSRF check expects.
req() {
  local method=$1 path=$2 jar=$3 body=${4:-}
  local args=(-sS -o "$work/body" -w '%{http_code}' -X "$method"
    -b "$jar" -c "$jar" -H "Origin: $origin")
  if [ -n "$body" ]; then
    args+=(-H 'Content-Type: application/json' --data "$body")
  fi
  curl "${args[@]}" "$base$path" || echo 000
}

expect() {
  local want=$1 got=$2 what=$3
  [ "$got" = "$want" ] || fail "$what: expected HTTP $want, got $got: $(head -c 500 "$work/body")"
}

wait_healthy() {
  local i
  for i in $(seq 1 60); do
    if curl -fsS "$base/health" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  return 1
}

# --- what to compare -------------------------------------------------------

if [ -z "${STARR_BIN:-}" ]; then
  log "building the current binary"
  (cd "$repo" && cargo build -q -p starr-server --locked)
  STARR_BIN="$repo/target/debug/starr"
fi
[ -x "$STARR_BIN" ] || fail "no executable at STARR_BIN=$STARR_BIN"

image=${ROLLBACK_IMAGE:-}
if [ -z "$image" ]; then
  # The newest release tag strictly before HEAD, so a tagged HEAD compares
  # against the release before it.
  prev_tag=$(git -C "$repo" describe --tags --abbrev=0 --match 'v*' HEAD^) ||
    fail "no previous v* tag found (fetch tags: git fetch --tags)"
  image="ghcr.io/bitoxo/starr:${prev_tag#v}"
fi
if ! docker image inspect "$image" >/dev/null 2>&1; then
  log "pulling $image"
  docker pull -q "$image" >/dev/null || fail "cannot pull $image (logged in to ghcr.io?)"
  pulled_image=$image
fi
echo "current:  $("$STARR_BIN" --version)"
echo "previous: $image"

# --- 1. current build: set up and fill a fresh database --------------------

log "starting the current build on a fresh data dir"
STARR_DATA_DIR="$data" STARR_BIND=127.0.0.1 STARR_PORT="$port" \
  STARR_PUBLIC_URL="$origin" STARR_LOG=info \
  "$STARR_BIN" >"$work/current.log" 2>&1 &
server_pid=$!
wait_healthy || fail "current build did not become healthy"

token=$(sed -n 's/.*SETUP TOKEN: *\([^ ]*\).*/\1/p' "$work/current.log" | head -n 1)
[ -n "$token" ] || fail "no setup token in the current build's log"

admin_jar="$work/admin.jar"
status=$(req POST /setup/admin "$admin_jar" \
  "$(jq -nc --arg t "$token" --arg u "$admin_user" --arg p "$admin_pass" \
    '{token: $t, username: $u, password: $p}')")
expect 201 "$status" "setup admin"
echo "admin created"

status=$(req POST /members "$admin_jar" \
  "$(jq -nc --arg u "$member_user" '{username: $u, email: ($u + "@example.com"),
    entitlements: [{category: "movies", enabled: true, requestMode: "auto"},
                   {category: "series", enabled: true, requestMode: "approval"}]}')")
expect 201 "$status" "create member"
link=$(jq -r '.setPasswordLink.url' "$work/body")
member_token=${link##*/}

member_jar="$work/member.jar"
status=$(req POST "/password-tokens/$member_token" "$member_jar" \
  "$(jq -nc --arg p "$member_pass" '{password: $p}')")
expect 200 "$status" "set member password"
echo "member created and password set"

status=$(req POST /invitations "$admin_jar" \
  '{"entitlements": [{"category": "music", "enabled": true, "requestMode": "approval"}]}')
expect 201 "$status" "create invitation"
echo "invitation created"

# A music source (spec 0006 slice 3): older releases must still list every
# category with it. The URL points nowhere; its check simply fails.
status=$(req POST /services "$admin_jar" \
  '{"adapterType": "lidarr", "name": "Rollback Lidarr", "baseUrl": "http://127.0.0.1:9", "apiKey": "rollback-lidarr-key-0123456789"}')
expect 202 "$status" "create Lidarr"
lidarr_id=$(jq -r '.service.id' "$work/body")
status=$(req PUT /categories/music/source "$admin_jar" \
  "$(jq -nc --argjson id "$lidarr_id" '{serviceId: $id}')")
expect 200 "$status" "set the music source"
status=$(req PUT /categories/music/source/settings "$admin_jar" '{}')
expect 200 "$status" "save music source settings"
echo "Lidarr set as the music source"

# Navidrome as music's consume service (spec 0006 slice 5): a login, not an
# API key, and a type older releases do not know. Points nowhere as well.
status=$(req POST /services "$admin_jar" \
  '{"adapterType": "navidrome", "name": "Rollback Navidrome", "baseUrl": "http://127.0.0.1:9", "username": "starr", "password": "rollback-navidrome-pass"}')
expect 202 "$status" "create Navidrome"
navidrome_id=$(jq -r '.service.id' "$work/body")
status=$(req POST /categories/music/roles "$admin_jar" \
  "$(jq -nc --argjson id "$navidrome_id" '{kind: "consume", serviceId: $id}')")
expect 201 "$status" "set Navidrome as music consume service"
echo "Navidrome set as the music consume service"

# Whisparr as the adult source (spec 0007 slice 3): a source role in a
# category older releases have no source for. Points nowhere as well; the
# category stays off.
status=$(req POST /services "$admin_jar" \
  '{"adapterType": "whisparr", "name": "Rollback Whisparr", "baseUrl": "http://127.0.0.1:9", "apiKey": "rollback-whisparr-key-0123456789"}')
expect 202 "$status" "create Whisparr"
whisparr_id=$(jq -r '.service.id' "$work/body")
status=$(req PUT /categories/adult/source "$admin_jar" \
  "$(jq -nc --argjson id "$whisparr_id" '{serviceId: $id}')")
expect 200 "$status" "set the adult source"
status=$(req PUT /categories/adult/source/settings "$admin_jar" '{}')
expect 200 "$status" "save adult source settings"
echo "Whisparr set as the adult source"

log "stopping the current build"
kill "$server_pid"
wait "$server_pid" 2>/dev/null || true
server_pid=
ls "$data"

# Adult scene requests (spec 0007 slice 4) as the current build stores them:
# a scene keyed by its StashDB UUID, the member's waiting request and an
# approved one with its acquisition at the Whisparr above. Written directly:
# requesting through the API would need a StashDB, and nothing is contacted.
log "adding adult scene requests"
python3 - "$data/starr.db" "$member_user" "$whisparr_id" <<'PY'
import sqlite3, sys

db, member, whisparr = sys.argv[1], sys.argv[2], int(sys.argv[3])
at = "2026-11-01T11:00:00.000Z"
con = sqlite3.connect(db)
cur = con.cursor()
(member_id,) = cur.execute("SELECT id FROM member WHERE username = ?", (member,)).fetchone()
for n, (title, state) in enumerate([("Scene A", "pending_approval"), ("Scene B", "submitted")]):
    cur.execute(
        "INSERT INTO catalog_item (kind, title, poster_path, created_at, updated_at)"
        " VALUES ('scene', ?, ?, ?, ?)",
        (title, f"stashdb:1a6e0000-0000-4000-8000-00000000000{n}", at, at),
    )
    item = cur.lastrowid
    cur.execute(
        "INSERT INTO catalog_key (catalog_item_id, scheme, value) VALUES (?, 'stashdb_scene', ?)",
        (item, f"5ce0e000-0000-4000-8000-00000000000{n}"),
    )
    cur.execute(
        "INSERT INTO request (member_id, catalog_item_id, category, state, created_at, updated_at)"
        " VALUES (?, ?, 'adult', ?, ?, ?)",
        (member_id, item, state, at, at),
    )
    request = cur.lastrowid
    acquisition = None
    if state == "submitted":
        cur.execute(
            "INSERT INTO acquisition (catalog_item_id, service_instance_id, category, state,"
            " quality_profile_id, root_folder_path, submitted_at, starr_added, starr_monitored,"
            " created_at, updated_at)"
            " VALUES (?, ?, 'adult', 'submitted', 3, '/data/private', ?, 1, 1, ?, ?)",
            (item, whisparr, at, at, at),
        )
        acquisition = cur.lastrowid
    cur.execute(
        "INSERT INTO request_item (request_id, member_id, catalog_item_id, decision,"
        " acquisition_id, decided_at)"
        " VALUES (?, ?, ?, ?, ?, ?)",
        (request, member_id, item, "approved" if acquisition else "pending_approval",
         acquisition, at if acquisition else None),
    )
    cur.execute(
        "INSERT INTO request_event (request_id, at, to_state, trigger, actor)"
        " VALUES (?, ?, ?, 'member', ?)",
        (request, at, state, member),
    )
con.commit()
con.close()
print("two adult scene requests added")
PY

# --- 2. previous release on the migrated database --------------------------

log "starting $image on the migrated data dir"
# Run as the invoking user so the data dir written by the current build stays
# readable and writable (the image defaults to uid 1000).
docker run -d --name "$container" \
  --user "$(id -u):$(id -g)" \
  -v "$data:/config" \
  -p "127.0.0.1:$port:7979" \
  -e STARR_PUBLIC_URL="$origin" \
  --health-interval=2s --health-start-period=10s --health-retries=3 \
  "$image" >/dev/null

health=
for _ in $(seq 1 60); do
  health=$(docker inspect -f '{{.State.Status}}/{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$container")
  case "$health" in
  running/healthy) break ;;
  running/*) sleep 1 ;;
  *) fail "previous release stopped: $health" ;;
  esac
done
[ "$health" = running/healthy ] || fail "previous release not healthy: $health"
echo "container healthy"

# --- 3. fatal checks --------------------------------------------------------

log "fatal checks"
status=$(req GET /health "$work/anon.jar")
expect 200 "$status" "GET /health"
echo "ok   GET /health"

for who in admin member; do
  user_var="${who}_user" pass_var="${who}_pass"
  jar="$work/$who-old.jar"
  status=$(req POST /auth/login "$jar" \
    "$(jq -nc --arg u "${!user_var}" --arg p "${!pass_var}" '{username: $u, password: $p}')")
  expect 200 "$status" "$who login"
  echo "ok   POST /auth/login ($who)"
  status=$(req GET /auth/me "$jar")
  expect 200 "$status" "$who GET /auth/me"
  me=$(jq -r '.username' "$work/body")
  [ "$me" = "${!user_var}" ] || fail "$who GET /auth/me: unexpected body $(head -c 500 "$work/body")"
  echo "ok   GET /auth/me ($who)"
done

status=$(req GET /categories "$work/admin-old.jar")
expect 200 "$status" "admin GET /categories"
music_source=$(jq -r '.[] | select(.category == "music") | .source.adapterType' "$work/body")
[ "$music_source" = lidarr ] || fail "GET /categories: music source missing: $(head -c 500 "$work/body")"
echo "ok   GET /categories (music source lidarr)"
adult_source=$(jq -r '.[] | select(.category == "adult") | .source.adapterType' "$work/body")
[ "$adult_source" = whisparr ] || fail "GET /categories: adult source missing: $(head -c 500 "$work/body")"
echo "ok   GET /categories (adult source whisparr)"

# The request lists with adult scene requests present (spec 0007 slice 4):
# the previous release leaves them out (it has no media kind for scenes) and
# must not fail the whole list.
for who_path in admin:/admin/requests admin:/admin/approvals admin:/admin/acquisitions \
  member:/requests; do
  who=${who_path%%:*} path=${who_path#*:}
  status=$(req GET "$path" "$work/$who-old.jar")
  expect 200 "$status" "$who GET $path with adult scene requests"
  echo "ok   GET $path ($who, adult scene requests present)"
done

# --- 4. non-fatal checks ----------------------------------------------------

log "non-fatal checks (known gap: older releases may not parse newer enum values)"
broken=0
check() {
  local who=$1 path=$2 status
  status=$(req GET "$path" "$work/$who-old.jar")
  if [ "$status" = 200 ]; then
    printf 'ok   %-28s (%s)\n' "GET $path" "$who"
  else
    broken=$((broken + 1))
    printf 'FAIL %-28s (%s) HTTP %s: %s\n' "GET $path" "$who" "$status" "$(head -c 200 "$work/body")"
  fi
}
for path in /members /invitations /admins /services /categories /admin/requests \
  /admin/approvals /admin/acquisitions /admin/notifications /runs /system/status; do
  check admin "$path"
done
for path in /me /me/sessions /requests /follows /notifications; do
  check member "$path"
done

log "result"
echo "fatal checks passed: the previous release serves login on the new schema"
if [ "$broken" -gt 0 ]; then
  echo "non-fatal: $broken endpoint(s) failed on the previous release (see above)"
else
  echo "non-fatal: all checked endpoints work on the previous release"
fi
