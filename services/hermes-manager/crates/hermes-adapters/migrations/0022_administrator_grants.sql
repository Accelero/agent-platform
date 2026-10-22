-- Keycloak authenticates exact identities; this table is the sole admin authority.
CREATE TABLE manager_admin_auth.verified_identities (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    issuer text COLLATE "C" NOT NULL,
    subject text COLLATE "C" NOT NULL,
    verified_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(installation_id,issuer,subject)
);
CREATE TABLE manager_admin_auth.administrator_grants (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    issuer text COLLATE "C" NOT NULL,
    subject text COLLATE "C" NOT NULL,
    active boolean NOT NULL,
    can_manage_administrators boolean NOT NULL,
    version bigint NOT NULL DEFAULT 1 CHECK(version>0),
    UNIQUE(installation_id,issuer,subject)
);
CREATE INDEX administrator_grant_order ON manager_admin_auth.administrator_grants(installation_id,id);
-- Session-only roles can test current grants without gaining writes to them.
CREATE FUNCTION manager_admin_auth.has_administrator_grant(installation uuid,identity_issuer text,identity_subject text)
RETURNS boolean LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
    SELECT EXISTS(SELECT 1 FROM manager_admin_auth.administrator_grants WHERE installation_id=installation AND issuer=identity_issuer AND subject=identity_subject AND active)
$$;
