-- Secret classes (ADR 0003 amendment 1): which key sealed a service
-- instance's credential. `service`: the worker's secret.key (write-capable
-- admin keys, opened only by the worker). `catalog`: the web-held web.key
-- (read-only catalog tokens such as TMDB's, opened by the web process too).
--
-- Existing rows were all sealed with secret.key, so they start as `service`;
-- the worker re-seals TMDB instances to the web key on its next start and
-- flips them to `catalog`.

ALTER TABLE service_instance
    ADD COLUMN credential_class TEXT NOT NULL DEFAULT 'service'
        CHECK (credential_class IN ('service', 'catalog'));
