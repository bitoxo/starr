# Configuration

<!-- Generated from crates/starr-server/src/config.rs by `just gen-config-docs`. Do not edit by hand. -->

Starr is configured through environment variables (the usual way in Docker) or the
equivalent command-line flags (`starr --help`). Invalid values stop the start with an
error message.

| Variable | Default | Description |
|---|---|---|
| `STARR_MODE` | `all` | Which parts this process runs: `all` (HTTP server and worker in one process, the default), `web` (HTTP only: enqueues jobs but never runs them and never reads `STARR_SECRET_KEY_FILE`) or `worker` (runs jobs, no HTTP listener). `web` and `worker` share `STARR_DATA_DIR` on a local volume. The subcommands `starr all`, `starr web` and `starr worker` take precedence over this variable |
| `STARR_DATA_DIR` | `./data` | Directory for the database, backups and other persistent state (`/config` in Docker) |
| `STARR_BIND` | `0.0.0.0` | Address to bind the HTTP server to |
| `STARR_PORT` | `7979` | HTTP port |
| `STARR_LOG` | `info` | Log filter in `tracing` syntax, e.g. `info,starr=debug` |
| `STARR_PUBLIC_URL` | – | Public base URL under which users reach Starr, e.g. `https://starr.example.com`. Absolute http(s) URL without path, query or fragment. Used for links (also in notification emails and webhooks, so in split mode give it to the worker too) and CSRF origin checks (mutating requests must send this `Origin` or one of `STARR_ADDITIONAL_ORIGINS`); setup refuses to finish without it. Development only: while unset, requests whose `Origin` matches their `Host` header also pass the CSRF check. Do not run without it in production |
| `STARR_ADDITIONAL_ORIGINS` | – | Further origins users reach Starr under, comma-separated, each like `STARR_PUBLIC_URL` (e.g. `http://192.168.1.10:7979` next to an https public URL). Accepted by the CSRF origin check |
| `STARR_BEHIND_PROXY` | `false` | Trust `X-Forwarded-*` headers from a reverse proxy in front of Starr. Enable only when Starr is reachable exclusively through that proxy. Starr trusts exactly one hop: the client IP (for login throttling) is the rightmost `X-Forwarded-For` entry, the one your proxy appended; entries left of it are client-supplied and ignored. The rightmost `X-Forwarded-Proto` decides the cookies: on `https` Starr uses `__Host-` prefixed `Secure` cookies, otherwise plain cookies without `Secure` (Starr itself only speaks http). The headers count only when the connection comes from a loopback or private address (`127.0.0.0/8`, `::1`, RFC 1918, link-local, IPv6 ULA `fc00::/7`), i.e. a proxy on the host or in a Docker network; from any other peer they are ignored |
| `STARR_ADMIN_TOTP_REQUIRED` | `false` | Require a second factor (TOTP) for every admin account. An admin without TOTP is sent to enrollment right after logging in; until it is confirmed, no other admin API works |
| `STARR_MEMBER_DELETE_GRACE_DAYS` | `30` | Days a deleted member stays restorable before only a tombstone remains (username reserved, everything else removed). Applies to members who have used Starr or own an account Starr created in a media server; members who never did are deleted at once. Their media server accounts are disabled at once; when this period ends the accounts Starr created are deleted and linked ones released (never ignored or administrator accounts). With 0 this happens at the next reconcile, without a disabled phase |
| `STARR_IMAGE_CACHE_MB` | `1024` | Size limit of the disk cache of proxied posters and logos, in MiB. The worker's hourly housekeeping deletes the least recently used images beyond it |
| `STARR_ADULT_IMAGE_CACHE_MB` | `256` | Size limit of the separate disk cache of the private category's images, in MiB. Its images are also deleted 30 days after they were last shown, and all at once when the category is switched off |
| `STARR_SECRET_KEY_FILE` | – | File holding the key that opens stored service credentials (age X25519). Created on first start with mode 0600. Any file not writable by group or others works (e.g. a read-only Docker secret, 0444). Starr refuses to start if the file is writable by group or others, unreadable or malformed. If it is missing or holds another key, Starr starts with that key and asks for the API key of every service sealed with the old one. Back it up together with `starr.db`. Defaults to `secret.key` inside `STARR_DATA_DIR` |
| `STARR_WEB_KEY_FILE` | – | File holding the web process's key for admins' TOTP secrets and catalog read tokens such as TMDB's (age X25519), created on first start like `STARR_SECRET_KEY_FILE` and with the same permission rules. The web side (`all` or `web` mode) creates and reads it; a `worker` process reads it if present (mount it read-only) to check catalog services and run catalog jobs, and never creates it. Back it up too. Defaults to `web.key` inside `STARR_DATA_DIR` |

## Time zone

Starr doesn't read `TZ`. It stores and logs times in UTC (log timestamps end in `Z`), and the web UI
shows times in each viewer's browser time zone. Setting `TZ` in the container does no harm.

## Reverse proxy and the private category

The private category's search sends its text in a request body (`POST /api/v1/private/search`),
never in a URL. Its pages and images still carry catalog ids in their paths
(`/api/v1/private/scenes/{id}`, `/api/v1/private/performers/{id}`,
`/api/v1/private/studios/{id}`, `/api/v1/images/stashdb/{size}/{id}`), and those ids identify
the titles. Starr never logs them; a reverse proxy in front of Starr may. Disable or filter
its access log for `/api/v1/private` and `/api/v1/images/stashdb` (for example nginx
`access_log off;` in a `location` for each, or a Caddy `log_skip` matcher).
