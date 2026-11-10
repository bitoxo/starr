# Recorded Lidarr 3.1.6.5079 responses

Read-only `GET`s against a live Lidarr 3.1.6.5079
(linuxserver.io image `3.1.6.5079-ls46`, SQLite, `/music` root), recorded
2026-11-03. No API key, host name or other credential is in these files.
The library held Coldplay (`cc197bad-dc9c-440d-a5b5-d52ba2e14234`, Lidarr
id 1, `added` the same day): unmonitored, `monitorNewItems: "none"`, quality profile 4,
metadata profile 1 ("Standard"), all 10 albums unmonitored, no files, empty
queue.

| File | Request (`/api/v1/...`) |
| --- | --- |
| `system_status.json` | `GET system/status` |
| `qualityprofile.json` | `GET qualityprofile` |
| `metadataprofile.json` | `GET metadataprofile` (Lidarr's defaults "Standard" and "None") |
| `rootfolder.json` | `GET rootfolder` |
| `artist.json` | `GET artist?mbId=cc197bad-dc9c-440d-a5b5-d52ba2e14234` |
| `artist_unknown.json` | `GET artist?mbId=00000000-0000-0000-0000-000000000000` → `[]` |
| `albums.json` | `GET album?artistId=1` — **trimmed**, see below |
| `album_lookup_existing.json` | `GET album/lookup?term=lidarr:61b7b6cd-86a6-4728-9478-b58c93bca8fd` (Mylo Xyloto, already in Lidarr) |
| `queue_details.json` | `GET queue/details?artistId=1` → `[]` |
| `queue_details_all.json` | `GET queue/details` → `[]` (the whole queue was empty) |

## Trimming

`albums.json` keeps 3 of the 10 recorded albums (ids 1, 2, 4: Mylo Xyloto,
Ghost Stories, Music of the Spheres), each entry complete and unchanged; the
full answer was 290 KB. `artist.json`'s `statistics.albumCount` (10) still
describes the full list. The removed Parachutes entry (id 10,
`1dc4c347-a1db-32aa-b14f-bc9cc507b843`) lives on, reshaped as a lookup
result, in `../album_lookup_parachutes.json`. Every other file is byte-for-byte as recorded.

## Not recordable read-only

Write replies, the duplicate-add 400, lookups of an artist or album Lidarr
does not have, and a queue with entries are in `../` and derived from
Lidarr's source and these shapes (see `../README.md`).
