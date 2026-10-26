-- Owned namespaces and PersistentVolumes without a registry record. The
-- controller replaces the report after every scan; nothing adopts or deletes
-- them.
CREATE TABLE manager.orphans (
    kind text NOT NULL CHECK (kind IN ('namespace', 'persistent_volume')),
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 253),
    uid text NOT NULL CHECK (length(uid) BETWEEN 1 AND 128),
    first_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (kind, name)
);

-- When the controller last completed a scan.
CREATE TABLE manager.orphan_scan (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    scanned_at timestamptz NOT NULL
);
