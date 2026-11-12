# Changelog

All notable changes to Starr. Versions follow [SemVer](https://semver.org/); before 1.0.0 minor versions may break.

## Unreleased

### Fixed
- Album cover grids load up to three times faster on a cold cache: only the request to the Cover Art Archive counts against its budget of 4 per second, no longer the two redirects to archive.org and its storage host.
- The music lane "Trending this week" shows each album once; an album listed twice in ListenBrainz's weekly statistics appeared twice.

## 0.4.2 — 2026-11-10 · Jellyfin 12.4

### Fixed
- Jellyfin 12.4 is accepted; account changes were refused as an untested version.

## 0.4.1 — 2026-11-10 · Security hardening

Fixes from a security review before the public launch. Admins will notice two things: moving a service to another server asks for its API key (or login) again, and a few sensitive changes ask for the password (and TOTP code) once more.

### Security
- A service's stored API key or login no longer follows a changed URL: when the scheme, host or port changes, the credential has to be entered again in the same change. Path changes on the same server keep it. The stored SMTP password likewise needs re-entering when the SMTP host or port changes.
- Changing a service's URL or credential, creating, suspending, reactivating or removing an admin, and adding a notification channel or changing where one sends (webhook URL, SMTP server or password) need a fresh re-authentication, like going live with account writes.
- A failed pre-migration backup no longer leaves an empty file that would count as the backup on the next start.
- Every response carries browser security headers: a strict Content-Security-Policy with `frame-ancestors 'none'`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Cross-Origin-Opener-Policy` and `Permissions-Policy`.
- With `STARR_BEHIND_PROXY=true`, `X-Forwarded-For` and `X-Forwarded-Proto` count only from a loopback or private peer; a client reaching port 7979 directly can no longer choose its throttling address or claim https.
- `starr.db`, its WAL and shared-memory files, pre-migration backups and the image cache are created 0600 (directories 0700). Existing files keep their permissions; `chmod 600 config/starr.db*` tightens an older install.
- `cargo deny`, `cargo audit` and `pnpm audit --prod` run in CI (`just audit`); three advisories without a fix are accepted with reasons in `deny.toml`.

### Changed
- The split compose example and the install guide's proxy snippets publish `127.0.0.1:7979:7979` with `STARR_BEHIND_PROXY=true`.
- The compose examples drop `TZ`: Starr doesn't read it.

## 0.4.0 — 2026-11-09 · Phases 3–4: music, private category · UI v1

Music joins movies and series: members request albums and follow artists, Lidarr acquires, Navidrome and the music libraries of Jellyfin/Emby report availability, and Navidrome accounts are provisioned like Jellyfin and Emby. An optional private category for adult content sits behind a privacy gate and stays invisible to members the admin has not entitled; admins see neutral rows until they unlock. The web UI is rebuilt from an audit of the 0.3.0 interface. 0.4.0 is the version the public launch starts from; 1.0 follows once Starr has run stably in real installs for a while.

### Added
- **Music catalog**: MusicBrainz for search, artists, albums and tracklists, ListenBrainz for Trending, Fresh releases and Popular, album covers through a Cover Art Archive proxy; no API key needed; Starr identifies itself with a fixed User-Agent and keeps to 1 request per second (#48, #49).
- **Lidarr source**: albums are added and monitored one by one, never the whole artist; a stop unmonitors exactly the album; metadata profile per category with a warning when it drops albums or EPs; tested against Lidarr 3.1.6 (#50, #51).
- **Album requests**: request one or more albums from the artist or album page, with the same approval, withdraw, revoke and writes-paused rules as seasons (#52).
- **Music availability and Navidrome**: availability and "Open in" from Navidrome and the music libraries of Jellyfin/Emby, matched by MusicBrainz release group; albums no music library lists show up in the admin Inbox (#54, #77).
- **Artist follows**: new albums of a followed artist are requested in the member's name; follow checks are spread over the day (#55).
- **Navidrome accounts**: listing, adoption, create, libraries, password and delete through the existing safe-mode gate; Navidrome has no disable flag, so disabling sets a random password and only the member's next password unlocks the account (#57).
- **Private category (adult)**: off by default; switching it on requires an operator statement; members see it only when entitled and after unlocking it for their session (it closes after 30 idle minutes); admins see neutral rows until they unlock; hidden rows answer "not found", never "forbidden" (#56).
- **StashDB catalog and Whisparr source** for the private category: StashDB accepts read-only keys only; Whisparr v3 (tested from 3.1.0.2088, below 4.0) is the only acquisition path; scene requests with the usual approval flow; performer and studio follows with a weekly cap per follow (default 10) (#58, #59, #61, #62).
- **Discover home**: a hero with the mixed top 10, cover rows with a preview card on hover and an info strip for keyboard and touch, search with category filters (#76, #89, #91, #93).
- **Category pages** for movies, series and music: top 10, "On \<provider\>", genres, top rated, new episodes this week, upcoming, music genre lanes ranked by ListenBrainz listens, and a filterable "See all" grid (#71, #75, #78).
- **Detail overlay** for movies, series, artists and albums, with tabs for seasons and episodes, tracks, cast, details and discography (#79).
- **Member pages**: Requests grouped by phase (on its way, waiting for approval, ready, closed) with a progress bar; a paged notification list; My services says what each account waits for (#80, #90, #92).
- **Private area** with its own browse, search, requests, following and scene pages (#82, #84).
- **Admin Inbox**: one list of what needs an admin (decide, fix, paused, setup) with a detail panel; undo for a fresh approval while nothing went to the source; Requests and Downloads as filterable tables (#77, #94).
- **Admin pages**: members, invitations and admins as searchable tables; own password and sessions; run filters by job and outcome; About lists data sources and licences (#81, #86).
- **Sign-in and onboarding**: "Forgot your password?" page, set-password links checked on load, a poster wall on the login page from the cached Trending lists (never the private category) (#83).
- `STARR_ADULT_IMAGE_CACHE_MB` (default 256) limits the separate image cache of the private category (#59).

### Changed
- New navigation: collapsible sidebar, bottom tab bar on phones, grouped admin sections; old admin URLs redirect (#73).
- One state vocabulary across chips, journeys, admin labels and notifications; requests not yet sent to their source show as "Queued" to admins (#74, #87).
- Admins confirm consequences before they happen: changing a source's URL, removing a source, a catalog or a watch-in server; source settings are kept and come back when that instance becomes the source again (#70, #87).
- The admin bell names how an approval turned out (approved, declined, withdrawn, revoked) (#87).
- Notifications of the private category are deleted after 90 days instead of 180; switching the category off purges its caches (#63).
- Webhook payloads name the private category `private`, never `adult`; email and Discord carry no category name (#64).
- The member bundle is split by route; pages other than Discover load on first use (#87).
- Adapters honour `X-RateLimit-*` headers and pause all callers until the window resets, at most 60 s (#75).
- The install guide covers building from source, the image tag scheme, reverse proxy exposure, volume ownership under rootless Docker and LXC, consistent backups, and that Starr logs in UTC (#95, #97).

### Fixed
- Focus returns to its opener after dialogs, the detail overlay and inline steps instead of dropping to the page; 44 px touch targets below 1024 px; no layout shift on load at 390 px; request rows wrap at 320 px (#74, #87).
- Table selection has its own column; row dividers stay continuous; dialogs fit the viewport with a scrolling body (#88).
- A chunk that fails to load (offline, or gone after an update) no longer takes the page down (#87).

### Security
- Every leak point of the private category is covered by tests: member API, admin views, audit entries, run reports, logs, image route, external notification channels; the private image cache logs no file paths and is served `no-store` (#56, #59, #63).
- Whisparr queue messages, which quote release names, leave the adapter only as a neutral hint (#58).
- Changing an admin's password ends their other sessions and pending logins; the current session cannot be revoked as "other" (#81).
- Private search text never appears in a URL; private ids still appear in paths, so keep `/api/v1/private` and `/api/v1/images/stashdb` out of reverse proxy access logs (see [`docs/configuration.md`](docs/configuration.md)) (#59).
- `?next=` after login only accepts relative in-app paths of the account's own area (#83).

### Known issues
- Music "Trending this week" can show the same album twice.
- Rolling back to 0.3.0 works on the 0.4.0 schema with limits: 0.3.0 does not show music requests, fails music acquisitions still pending and drops pending album stops (unmonitor those albums in Lidarr by hand), reports Navidrome as failed, and if it toggles the adult category the operator statement becomes invalid, so the private area stays closed until it is confirmed again (#52, #54, #56).

## 0.3.0 — 2026-11-03 · Phase 2: identity

Starr manages its members' accounts on Jellyfin and Emby: one password, the right libraries per category, suspend and delete in one place. Writes to media servers are off by default and go live per server only after an explicit, TOTP-protected confirmation.

### Added
- **Accounts overview**: Starr scans the accounts on every Jellyfin and Emby instance every 15 minutes, read-only, and marks admins as privileged (#38).
- **Libraries per category**: map each Jellyfin/Emby library to one category; unmapped libraries show up in "Needs you"; availability, "Open in" and removal detection only count a category's own libraries, so an adult title never counts as an available movie (#41).
- **Adoption and planner**: a members × servers matrix with suggested links to existing accounts, decisions per cell (create, link, none), invitations that name existing accounts, and a per-server plan of what Starr would change and why (#42).
- **Account writes with safe mode**: enable/disable accounts and set their libraries via read-modify-write of the full policy with read-back; plan-only by default per server, go live / pause / resume / approve with elevation; at most five destructive steps per server and hour, larger plans wait for approval (#44, #46).
- **Member passwords and My services**: the member's Starr password is handed sealed to the worker and set on their accounts; members activate their accounts or switch an existing one to the Starr password on "My services"; new accounts are created only together with a password (#45).
- **Lifecycle**: suspending a member disables their accounts; deleting a member deletes accounts Starr created after a 30-day grace period, adopted accounts are disabled and released, never deleted (#46).
- **Drift and conflicts**: changes made directly on a media server (admin rights gained, locked by the service, renamed, gone, stale library mappings) appear in "Needs you"; admins are notified about privileged and locked accounts (#46).
- **Removing requests** (spec 0004): members withdraw, admins revoke with a reason; Starr only stops monitoring and never deletes files (#37).
- **Catalog identity** (ADR 0012): catalog keys per scheme and the categories music and adult prepared for phases 3 and 4 (#40).

### Security
- Linking accounts, going live and deleting members with accounts require re-entering the password; going live, resuming and approving require TOTP for every active admin, and disabling TOTP pauses all live servers (#44, #45, #46).
- Passwords never appear in logs, audit entries, job payloads, errors or API responses; only the Argon2 hash is stored, the hand-off is sealed for the worker and deleted after use or one hour (#45).
- A job retry never writes the same password twice, so devices are not logged out again (#46).

### Changed
- CI checks that the previous release image still serves on the new schema (rollback compatibility, ADR 0008) (#39).

## 0.2.0 — 2026-11-02 · Phase 1: movies & series

Members discover, search and request movies and series; admins approve; Radarr/Sonarr acquire; members follow progress and open titles in Jellyfin or Emby.

### Added
- **Catalog**: TMDB adapter (trending, popular, upcoming, on your services, top rated, search, details with trailer) with relevance filters per category and an image proxy with disk cache; the browser never contacts TMDB (#19).
- **Categories**: catalog/source/watch-in roles per category, relevance settings with live Trending preview, quality profile, root folder and minimum availability per category; two secret classes (service credentials worker-only, catalog read tokens sealed with the web key, ADR 0003 amendment 1) (#20).
- **Sources**: Radarr and Sonarr adapters (lookup, add, monitor seasons, queue progress, profiles/root folders, read-back after writes) (#17).
- **Watch in**: Jellyfin and Emby read adapters, inventory sync with completeness evidence, "Open in" deep links via each instance's public URL (#21).
- **Requests**: request lifecycle with approval or auto mode per member and category, shared acquisitions, withdraw, reject with reason, re-requests after a rejection always need approval, admin "Needs you" and request/acquisition overviews, submit and sync jobs (#23).
- **Member UI**: Discover swimlanes, search, detail sheet with season selection, Requests page (#24).
- **Follows**: follow a series and get new seasons requested in your name by a daily job (#26).
- **Notifications**: in-app bell for members and admins, email (SMTP) for members, webhook and Discord for admins, delivery with retries (#27, #32).
- **Writes paused** per Radarr/Sonarr instance: approvals are held until resumed, with a confirmation listing what will be sent (#28, #32).
- **Status journey card** behind every status chip: where a request stands and what comes next (#30).

### Fixed
- First run sends visitors to setup instead of the login form (#22).
- Write transactions take the write lock up front, so concurrent worker writes no longer fail with `SQLITE_BUSY_SNAPSHOT` (#25).
- Downloaded titles waiting for Jellyfin/Emby show "Importing" / "Almost ready" instead of "Downloading 100%", and the library is refreshed early (#29).

### Changed
- Release images build natively per platform (amd64, arm64) in about five minutes instead of two hours under emulation (#31).

## 0.1.0 — 2026-11-01 · Phase 0: foundation

First-run setup, admin and member accounts, invitations, sessions with TOTP, service instances with sealed credentials, job queue with runs, split mode (`starr web` / `starr worker`), Docker image.
