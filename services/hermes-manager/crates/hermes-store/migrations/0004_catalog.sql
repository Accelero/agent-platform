-- Versioned catalogs: Hermes templates and terminal backends.
CREATE TABLE manager.catalog_versions (
    kind text NOT NULL CHECK (kind IN ('template', 'terminal_backend')),
    id text NOT NULL CHECK (id ~ '^[a-z]([-a-z0-9]{0,61}[a-z0-9])?$'),
    version bigint NOT NULL CHECK (version >= 1),
    state text NOT NULL CHECK (state IN ('draft', 'published', 'withdrawn')),
    revision bigint NOT NULL CHECK (revision >= 1),
    definition jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (kind, id, version)
);

-- Published definitions never change; states move forward only; revisions
-- advance by one; nothing is deleted.
CREATE FUNCTION manager.guard_catalog_version() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.kind <> OLD.kind OR NEW.id <> OLD.id OR NEW.version <> OLD.version
        OR NEW.revision <> OLD.revision + 1
        OR (OLD.state <> 'draft' AND NEW.definition <> OLD.definition)
        OR NOT ((OLD.state = 'draft' AND NEW.state IN ('draft', 'published'))
                OR (OLD.state = 'published' AND NEW.state = 'withdrawn'))
    THEN
        RAISE EXCEPTION 'manager: catalog versions only move forward';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER catalog_version_forward BEFORE UPDATE ON manager.catalog_versions
    FOR EACH ROW EXECUTE FUNCTION manager.guard_catalog_version();
CREATE TRIGGER catalog_version_no_delete BEFORE DELETE ON manager.catalog_versions
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
