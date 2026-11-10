-- Service instances (spec 0002 slice 7, ADR 0003, ADR 0009): the services
-- Starr talks to. The credential is sealed with age; `credential_recipient`
-- names the public key it was sealed for (the X25519 ciphertext does not),
-- so startup can refuse a key file that cannot open stored credentials.

CREATE TABLE service_instance (
    id                   INTEGER PRIMARY KEY,
    -- Adapter type id from the adapter's descriptor, e.g. `radarr`.
    adapter_type         TEXT NOT NULL,
    name                 TEXT NOT NULL UNIQUE COLLATE NOCASE,
    base_url             TEXT NOT NULL,
    credential_sealed    BLOB NOT NULL,
    credential_recipient TEXT NOT NULL,
    -- `probe`: Starr only checks the connection; `live` arrives with phase 1.
    mode                 TEXT NOT NULL DEFAULT 'probe' CHECK (mode IN ('probe', 'live')),
    -- Facts from the last successful probe (lesson 32: read, not remembered).
    detected_version     TEXT,
    -- JSON array of capability names the instance offers.
    capabilities         TEXT CHECK (capabilities IS NULL OR json_valid(capabilities)),
    last_check_at        TEXT,
    -- Error taxonomy code of the last check; NULL when it succeeded.
    last_error           TEXT,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
) STRICT;

CREATE INDEX service_instance_credential_recipient ON service_instance (credential_recipient);
