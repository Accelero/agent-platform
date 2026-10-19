-- Separate authentication schemas can be granted to separate database roles.
-- Router authentication needs no USAGE or DML rights on manager assignments.
CREATE SCHEMA manager_admin_auth;
CREATE SCHEMA manager_router_auth;
REVOKE ALL ON SCHEMA manager_admin_auth, manager_router_auth FROM PUBLIC;

CREATE TABLE manager_admin_auth.scope (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    installation_id uuid NOT NULL,
    issuer text COLLATE "C" NOT NULL,
    epoch bigint NOT NULL CHECK (epoch > 0),
    mode text NOT NULL CHECK (mode IN ('active','maintenance','restore')),
    schema_version bigint NOT NULL DEFAULT 1 CHECK (schema_version = 1)
);
CREATE TABLE manager_router_auth.scope (LIKE manager_admin_auth.scope INCLUDING ALL);

CREATE TABLE manager_admin_auth.browsers (
    installation_id uuid NOT NULL,
    epoch bigint NOT NULL CHECK (epoch > 0),
    id_hash bytea NOT NULL CHECK (octet_length(id_hash)=32),
    generation bigint NOT NULL CHECK (generation > 0),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (installation_id,epoch,id_hash)
);
CREATE TABLE manager_router_auth.browsers (LIKE manager_admin_auth.browsers INCLUDING ALL);

CREATE TABLE manager_admin_auth.flows (
    installation_id uuid NOT NULL,
    epoch bigint NOT NULL CHECK (epoch > 0),
    state_hash bytea NOT NULL CHECK (octet_length(state_hash)=32),
    browser_hash bytea NOT NULL CHECK (octet_length(browser_hash)=32),
    browser_generation bigint NOT NULL CHECK (browser_generation > 0),
    key_id text NOT NULL CHECK (length(key_id) BETWEEN 1 AND 63),
    nonce bytea NOT NULL CHECK (octet_length(nonce)=24),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 16 AND 65536),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    session_hash bytea CHECK (session_hash IS NULL OR octet_length(session_hash)=32),
    PRIMARY KEY (installation_id,epoch,state_hash),
    FOREIGN KEY (installation_id,epoch,browser_hash) REFERENCES manager_admin_auth.browsers(installation_id,epoch,id_hash)
);
CREATE TABLE manager_router_auth.flows (LIKE manager_admin_auth.flows INCLUDING ALL);
ALTER TABLE manager_router_auth.flows ADD FOREIGN KEY (installation_id,epoch,browser_hash)
    REFERENCES manager_router_auth.browsers(installation_id,epoch,id_hash);

CREATE TABLE manager_admin_auth.sessions (
    installation_id uuid NOT NULL,
    epoch bigint NOT NULL CHECK (epoch > 0),
    id_hash bytea NOT NULL CHECK (octet_length(id_hash)=32),
    browser_hash bytea NOT NULL CHECK (octet_length(browser_hash)=32),
    browser_generation bigint NOT NULL CHECK (browser_generation > 0),
    issuer text COLLATE "C" NOT NULL,
    subject text COLLATE "C" NOT NULL CHECK (octet_length(subject) BETWEEN 1 AND 255),
    key_id text NOT NULL CHECK (length(key_id) BETWEEN 1 AND 63),
    nonce bytea NOT NULL CHECK (octet_length(nonce)=24),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 16 AND 65536),
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active','refreshing','revoked')),
    verified_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    refresh_owner uuid,
    refresh_until timestamptz,
    fence bigint NOT NULL DEFAULT 0 CHECK (fence >= 0),
    CHECK ((state='refreshing')=(refresh_owner IS NOT NULL)),
    CHECK ((refresh_owner IS NULL)=(refresh_until IS NULL)),
    PRIMARY KEY (installation_id,epoch,id_hash),
    FOREIGN KEY (installation_id,epoch,browser_hash) REFERENCES manager_admin_auth.browsers(installation_id,epoch,id_hash)
);
CREATE TABLE manager_router_auth.sessions (LIKE manager_admin_auth.sessions INCLUDING ALL);
ALTER TABLE manager_router_auth.sessions ADD FOREIGN KEY (installation_id,epoch,browser_hash)
    REFERENCES manager_router_auth.browsers(installation_id,epoch,id_hash);
CREATE INDEX admin_flows_browser ON manager_admin_auth.flows(installation_id,epoch,browser_hash);
CREATE INDEX router_flows_browser ON manager_router_auth.flows(installation_id,epoch,browser_hash);
CREATE INDEX admin_sessions_browser ON manager_admin_auth.sessions(installation_id,epoch,browser_hash);
CREATE INDEX router_sessions_browser ON manager_router_auth.sessions(installation_id,epoch,browser_hash);
CREATE INDEX admin_flows_expiry ON manager_admin_auth.flows(expires_at);
CREATE INDEX router_flows_expiry ON manager_router_auth.flows(expires_at);
CREATE INDEX admin_sessions_expiry ON manager_admin_auth.sessions(expires_at);
CREATE INDEX router_sessions_expiry ON manager_router_auth.sessions(expires_at);
CREATE INDEX admin_sessions_revoked ON manager_admin_auth.sessions(state) WHERE state='revoked';
CREATE INDEX router_sessions_revoked ON manager_router_auth.sessions(state) WHERE state='revoked';
CREATE INDEX admin_browsers_expiry ON manager_admin_auth.browsers(expires_at);
CREATE INDEX router_browsers_expiry ON manager_router_auth.browsers(expires_at);

-- A coordinator changes the installation epoch once registry/data recovery is
-- verified. Authentication readers see that change without assignment access;
-- old process configurations and old session records immediately fail closed.
CREATE FUNCTION manager.sync_auth_scope() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    IF TG_OP='UPDATE' AND (NEW.id<>OLD.id OR NEW.issuer<>OLD.issuer) THEN
        RAISE EXCEPTION 'immutable installation ownership' USING ERRCODE='23514';
    END IF;
    IF TG_OP='UPDATE' AND NEW.epoch<OLD.epoch THEN
        RAISE EXCEPTION 'installation epoch regression' USING ERRCODE='23514';
    END IF;
    INSERT INTO manager_admin_auth.scope(singleton,installation_id,issuer,epoch,mode)
        VALUES(true,NEW.id,NEW.issuer,NEW.epoch,NEW.mode)
        ON CONFLICT(singleton) DO UPDATE SET installation_id=NEW.id,issuer=NEW.issuer,epoch=NEW.epoch,mode=NEW.mode;
    INSERT INTO manager_router_auth.scope(singleton,installation_id,issuer,epoch,mode)
        VALUES(true,NEW.id,NEW.issuer,NEW.epoch,NEW.mode)
        ON CONFLICT(singleton) DO UPDATE SET installation_id=NEW.id,issuer=NEW.issuer,epoch=NEW.epoch,mode=NEW.mode;
    RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION manager.sync_auth_scope() FROM PUBLIC;
CREATE TRIGGER installation_auth_scope AFTER INSERT OR UPDATE ON manager.installations
    FOR EACH ROW EXECUTE FUNCTION manager.sync_auth_scope();
INSERT INTO manager_admin_auth.scope(singleton,installation_id,issuer,epoch,mode)
    SELECT true,id,issuer,epoch,mode FROM manager.installations;
INSERT INTO manager_router_auth.scope(singleton,installation_id,issuer,epoch,mode)
    SELECT true,id,issuer,epoch,mode FROM manager.installations;
REVOKE ALL ON ALL TABLES IN SCHEMA manager_admin_auth, manager_router_auth FROM PUBLIC;

-- Narrow definer functions let session-only roles hold the epoch read lock
-- without granting UPDATE on scope (required by SELECT FOR SHARE).
CREATE FUNCTION manager_admin_auth.check_scope(uuid,bigint,text) RETURNS text
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE current_mode text;
BEGIN
    SELECT mode INTO current_mode FROM manager_admin_auth.scope
        WHERE singleton AND installation_id=$1 AND epoch=$2 AND issuer=$3 AND schema_version=1 FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'authentication recovery required' USING ERRCODE='P0001'; END IF;
    RETURN current_mode;
END;
$$;
CREATE FUNCTION manager_router_auth.check_scope(uuid,bigint,text) RETURNS text
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE current_mode text;
BEGIN
    SELECT mode INTO current_mode FROM manager_router_auth.scope
        WHERE singleton AND installation_id=$1 AND epoch=$2 AND issuer=$3 AND schema_version=1 FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'authentication recovery required' USING ERRCODE='P0001'; END IF;
    RETURN current_mode;
END;
$$;
REVOKE ALL ON FUNCTION manager_admin_auth.check_scope(uuid,bigint,text),manager_router_auth.check_scope(uuid,bigint,text) FROM PUBLIC;
