-- Follows (domain model §6, spec 0003 D8, slice 7).
--
-- follow: personal. One member follows one series in one category: new
-- seasons become requests in the member's name with their request mode
-- (`follows.check`), never silent monitoring in the source. A follow
-- starts at a cutoff: `seasons_through` is the highest regular season that
-- had aired when it started; only later seasons are new (they count once
-- the catalog lists them with an air date, D8). `follows.check` moves the cutoff past every new season it handled
-- (requested, or skipped as available or already requested), in the same
-- transaction as the request it created, so a season is acted on once.
--
-- Unfollowing ends a follow (`ended_at`) instead of deleting it; at most
-- one follow per member and series is active (lesson 4: the owner is part
-- of the rule). Following again starts a new follow with a new cutoff.
--
-- request.follow stays as recorded ("asked with follow", or created by a
-- follow); the follow table is what `follows.check` acts on. Requests made
-- with follow before this migration get their follow here, with the
-- seasons known now (and the seasons they asked for) as the cutoff.

CREATE TABLE follow (
    id              INTEGER PRIMARY KEY,
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    -- The series.
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item (id),
    category        TEXT NOT NULL CHECK (category IN ('movies', 'series', 'music', 'adult')),
    seasons_through INTEGER NOT NULL CHECK (seasons_through >= 0),
    created_at      TEXT NOT NULL,
    -- The last run of `follows.check` that read the catalog for it.
    checked_at      TEXT,
    ended_at        TEXT,
    CHECK (ended_at IS NULL OR ended_at >= created_at)
) STRICT;

CREATE UNIQUE INDEX follow_one_active ON follow (member_id, catalog_item_id)
    WHERE ended_at IS NULL;
CREATE INDEX follow_member ON follow (member_id, created_at);
CREATE INDEX follow_catalog_item ON follow (catalog_item_id);
CREATE INDEX follow_active ON follow (id) WHERE ended_at IS NULL;

INSERT INTO follow (member_id, catalog_item_id, category, seasons_through, created_at)
SELECT r.member_id, r.catalog_item_id, min(r.category),
       max(
           coalesce((SELECT max(s.season_number) FROM catalog_item s
                     WHERE s.parent_id = r.catalog_item_id AND s.kind = 'season'
                       AND s.season_number > 0
                       AND json_extract(s.metadata, '$.airDate') <= date('now')), 0),
           coalesce((SELECT max(i.season_number) FROM request_item i
                     JOIN request q ON q.id = i.request_id
                     WHERE q.member_id = r.member_id AND q.catalog_item_id = r.catalog_item_id
                       AND q.follow = 1 AND q.state NOT IN ('cancelled', 'rejected')), 0)
       ),
       min(r.created_at)
FROM request r
JOIN catalog_item c ON c.id = r.catalog_item_id AND c.kind = 'series'
-- A withdrawn or rejected request took its "follow new seasons" with it.
WHERE r.follow = 1 AND r.state NOT IN ('cancelled', 'rejected')
GROUP BY r.member_id, r.catalog_item_id;
