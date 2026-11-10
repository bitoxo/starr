# Security

Starr holds API keys to your Radarr, Sonarr, Lidarr, Whisparr, Jellyfin, Emby and Navidrome, and it is often reachable from the internet. Security reports are the most useful thing you can send.

## Reporting a vulnerability

Please report privately through GitHub: **[Report a vulnerability](https://github.com/bitoxo/starr/security/advisories/new)** (Security tab → Advisories). Do not open a public issue or discussion.

Include what you found, how to reproduce it, the Starr version (or commit) and your setup (single container or web/worker split, reverse proxy). A proof of concept helps but isn't required.

What to expect: Starr has one maintainer, so this is best effort. I aim to acknowledge a report within a week, agree on a fix and a disclosure date with you, and credit you in the advisory and the changelog unless you'd rather not be named.

Independent review of Starr's security design and code is welcome; this page is the overview. Ask about a specific part in Discussions or through an advisory.

## Supported versions

Only the latest release gets security fixes.

## Scope

In scope: the Starr server and worker, the web UI, the container image, and the documented deployment examples. Examples of what matters most:

- reading or using a service API key, a member's password or a TOTP secret without authorisation
- bypassing login, two-factor, re-authentication, CSRF checks or the plan-only mode for media server accounts
- titles, names or images from the private category reaching a notification channel or a log, or its existence reaching a member who isn't allowed to see it
- getting Starr to send requests to hosts it shouldn't (SSRF), or to write to a service in a way the admin didn't confirm

Out of scope: vulnerabilities in Radarr, Sonarr, Jellyfin and the other services themselves (report them upstream), attacks that need an admin's session or shell access to the host, and missing hardening of your own reverse proxy.

## What Starr stores

Everything lives in `/config` (or the key files you point to with `STARR_SECRET_KEY_FILE` and `STARR_WEB_KEY_FILE`):

| What | How |
|---|---|
| Admin and member passwords | Argon2 hashes only |
| Session, invitation and set-password tokens, TOTP recovery codes | SHA-256 hashes only |
| Service credentials (API keys, the Navidrome admin password), SMTP password, webhook and Discord URLs | Encrypted with `secret.key` (age, X25519). In the web/worker split only the worker holds it; the web container can seal new values but never open them. A sealed value isn't bound to its URL: a compromised web container can't read a stored key, but it can write `starr.db` and so point a service at another host for the worker to send the key to. Through the API, moving a service needs its key again (below) |
| Admins' TOTP secrets, catalog read tokens (TMDB, StashDB) | Encrypted with `web.key`. Starr refuses a StashDB key whose account can do more than read and vote |
| A member's new password on its way to the media servers | Sealed for the worker, deleted after use or after one hour |
| Members, invitations, requests, follows, notifications, audit log | Plain rows in `starr.db` |
| Catalog responses and images | Cache in `starr.db` and on disk, size-limited; private category images in a separate cache that is emptied when the category is switched off |

Passwords and keys never appear in logs, audit entries, job payloads, error messages or API responses.

Moving a service to another server (a different scheme, host or port) needs its API key or login typed again in the same change, so a stored key never follows the URL to a host it wasn't entered for; the same holds for the SMTP password when the SMTP host or port changes. These changes, a new key, creating, suspending, reactivating or removing an admin, and adding or redirecting a notification channel also need a fresh re-authentication (password, plus TOTP when it is on), valid for five minutes.

## Browser and proxy hardening

Every response carries a strict `Content-Security-Policy` (scripts, styles, fonts, images and requests from Starr's own origin only, no framing via `frame-ancestors 'none'`), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`, `Cross-Origin-Opener-Policy: same-origin` and a restrictive `Permissions-Policy`. HSTS is left to your TLS proxy.

With `STARR_BEHIND_PROXY=true` Starr reads `X-Forwarded-For` and `X-Forwarded-Proto` only from a loopback or private peer address (a proxy on the host or in a Docker network); from any other peer they are ignored. Publish port 7979 only to the proxy, as the [README](README.md#reverse-proxy) shows.

`starr.db` with its WAL files, the pre-migration backups and the image cache are created readable by Starr's user only (0600, directories 0700); files from older versions keep their permissions.

## What Starr connects to

Starr sends no telemetry, has no update check and no analytics. Outbound connections go to:

| Host | When |
|---|---|
| The services you connect (Radarr, Sonarr, Lidarr, Whisparr, Jellyfin, Emby, Navidrome) | At the URLs you enter |
| `api.themoviedb.org` | Movies and series: discover, search, details (with a TMDB token you provide) |
| `image.tmdb.org` | Posters, backdrops and logos for Starr's image proxy |
| `musicbrainz.org` | Music catalog, about one request per second at most, as MusicBrainz asks |
| `coverartarchive.org`, `archive.org` and its storage hosts | Album covers |
| `api.listenbrainz.org` | Music charts and genre ranking on Discover |
| Your stash-box (`stashdb.org` and its subdomains by default) | Only if you switch on the private category, with your own key |
| Your SMTP server, webhook URLs, Discord | Only if you configure notifications |

Every HTTP request to a catalog or service carries the User-Agent `Starr/<version> ( https://github.com/bitoxo/starr )`, as MusicBrainz requires. Nothing about you or your members is in it.

Your members' browsers only talk to Starr. Posters, covers and logos go through Starr's image proxy, and fonts are bundled. External links (trailers on YouTube, the JustWatch credit, attribution links, "Open in" your servers) open only when someone clicks them.
