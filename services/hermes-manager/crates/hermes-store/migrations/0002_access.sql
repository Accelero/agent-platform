-- Identities and administrator grants. Grants are the only administrator authority.
CREATE TABLE manager.identities (
    id uuid PRIMARY KEY,
    issuer text NOT NULL,
    subject text NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    display_name text,
    email text,
    -- Set by the first successful login; grant targets need it.
    verified_at timestamptz,
    UNIQUE (issuer, subject)
);

CREATE FUNCTION manager.guard_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.issuer <> OLD.issuer OR NEW.subject <> OLD.subject THEN
        RAISE EXCEPTION 'manager: identity keys are immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER identity_keys BEFORE UPDATE ON manager.identities
    FOR EACH ROW EXECUTE FUNCTION manager.guard_identity();
CREATE TRIGGER identity_no_delete BEFORE DELETE ON manager.identities
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE TABLE manager.administrator_grants (
    identity_id uuid PRIMARY KEY REFERENCES manager.identities (id),
    active boolean NOT NULL,
    can_manage_administrators boolean NOT NULL,
    version bigint NOT NULL CHECK (version >= 1),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE FUNCTION manager.guard_grant_version() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.version <> OLD.version + 1 OR NEW.identity_id <> OLD.identity_id THEN
        RAISE EXCEPTION 'manager: grant versions advance by one';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER grant_version BEFORE UPDATE ON manager.administrator_grants
    FOR EACH ROW EXECUTE FUNCTION manager.guard_grant_version();
CREATE TRIGGER grant_no_delete BEFORE DELETE ON manager.administrator_grants
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
