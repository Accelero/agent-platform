-- Admin secrets: sealed values, one row per version. Versions never change;
-- a whole secret is deleted only when nothing references it.
CREATE TABLE manager.admin_secrets (
    name text NOT NULL CHECK (name ~ '^[a-z]([-a-z0-9]{0,61}[a-z0-9])?$'),
    version bigint NOT NULL CHECK (version >= 1),
    kind text NOT NULL CHECK (kind IN ('env', 'file', 'registry')),
    sealed bytea NOT NULL CHECK (octet_length(sealed) BETWEEN 1 AND 16384),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (name, version)
);
CREATE TRIGGER admin_secret_versions_immutable BEFORE UPDATE ON manager.admin_secrets
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
