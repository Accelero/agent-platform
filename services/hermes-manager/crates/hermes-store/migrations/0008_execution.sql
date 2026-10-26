-- Terminal execution: environments (one worker Pod each), leases and
-- idempotent acquire requests.

-- The execution API finds an instance by its credential digest.
CREATE UNIQUE INDEX materials_execution_credential ON manager.materials (public)
    WHERE purpose = 'execution-credential';

CREATE TABLE manager.environments (
    id uuid PRIMARY KEY,
    instance_id uuid NOT NULL REFERENCES manager.instances (id),
    backend_kind text NOT NULL DEFAULT 'terminal_backend' CHECK (backend_kind = 'terminal_backend'),
    backend_id text NOT NULL,
    backend_version bigint NOT NULL,
    execution_generation bigint NOT NULL CHECK (execution_generation >= 1),
    mode text NOT NULL CHECK (mode IN ('per_session', 'per_profile_on_demand', 'per_profile_always_running')),
    key text NOT NULL CHECK (length(key) BETWEEN 1 AND 4200),
    invocation jsonb,
    state text NOT NULL CHECK (state IN ('pending', 'starting', 'ready', 'draining', 'retired')),
    pod_uid text CHECK (pod_uid IS NULL OR length(pod_uid) BETWEEN 1 AND 128),
    ssh_host text CHECK (ssh_host IS NULL OR length(ssh_host) BETWEEN 1 AND 64),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_used_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    drain_deadline timestamptz,
    FOREIGN KEY (backend_kind, backend_id, backend_version)
        REFERENCES manager.catalog_versions (kind, id, version)
);
-- One live environment per instance, backend version, generation and key.
CREATE UNIQUE INDEX environments_live ON manager.environments
    (instance_id, backend_id, backend_version, execution_generation, mode, key) WHERE state <> 'retired';
CREATE INDEX environments_by_instance ON manager.environments (instance_id, created_at, id);

-- Domain rule: environment states only move forward, the Pod UID and SSH
-- address are recorded once, and retired is final.
CREATE FUNCTION manager.guard_environment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.instance_id <> OLD.instance_id OR NEW.backend_id <> OLD.backend_id
        OR NEW.backend_version <> OLD.backend_version OR NEW.execution_generation <> OLD.execution_generation
        OR NEW.mode <> OLD.mode OR NEW.key <> OLD.key OR NEW.invocation IS DISTINCT FROM OLD.invocation
        OR (OLD.pod_uid IS NOT NULL AND NEW.pod_uid IS DISTINCT FROM OLD.pod_uid)
        OR (OLD.ssh_host IS NOT NULL AND NEW.ssh_host IS DISTINCT FROM OLD.ssh_host)
        OR NOT (NEW.state = OLD.state
                OR (OLD.state = 'pending' AND NEW.state IN ('starting', 'draining'))
                OR (OLD.state = 'starting' AND NEW.state IN ('ready', 'draining'))
                OR (OLD.state = 'ready' AND NEW.state = 'draining')
                OR (OLD.state = 'draining' AND NEW.state = 'retired'))
        OR (OLD.state = 'retired' AND NEW IS DISTINCT FROM OLD)
    THEN
        RAISE EXCEPTION 'manager: environments only move forward';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER environment_forward BEFORE UPDATE ON manager.environments
    FOR EACH ROW EXECUTE FUNCTION manager.guard_environment();
CREATE TRIGGER environment_no_delete BEFORE DELETE ON manager.environments
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE TABLE manager.leases (
    id uuid PRIMARY KEY,
    instance_id uuid NOT NULL REFERENCES manager.instances (id),
    environment_id uuid NOT NULL REFERENCES manager.environments (id),
    parent uuid REFERENCES manager.leases (id),
    state text NOT NULL CHECK (state IN ('active', 'released', 'expired')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    ended_at timestamptz
);
CREATE INDEX leases_live ON manager.leases (environment_id) WHERE state = 'active';
CREATE INDEX leases_due ON manager.leases (expires_at) WHERE state = 'active';

-- Domain rule: released and expired leases are final and never revive.
CREATE FUNCTION manager.guard_lease() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.instance_id <> OLD.instance_id OR NEW.environment_id <> OLD.environment_id
        OR NEW.parent IS DISTINCT FROM OLD.parent
        OR (OLD.state <> 'active' AND NEW IS DISTINCT FROM OLD)
        OR (NEW.state = 'active' AND OLD.state <> 'active')
    THEN
        RAISE EXCEPTION 'manager: ended leases are final';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER lease_final BEFORE UPDATE ON manager.leases
    FOR EACH ROW EXECUTE FUNCTION manager.guard_lease();
CREATE TRIGGER lease_no_delete BEFORE DELETE ON manager.leases
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE TABLE manager.acquire_requests (
    instance_id uuid NOT NULL REFERENCES manager.instances (id),
    request text NOT NULL CHECK (length(request) BETWEEN 1 AND 256),
    body_digest bytea NOT NULL CHECK (octet_length(body_digest) = 32),
    environment_id uuid NOT NULL REFERENCES manager.environments (id),
    lease_id uuid REFERENCES manager.leases (id),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (instance_id, request)
);

-- Domain rule: a request identifier keeps its body, environment and lease.
CREATE FUNCTION manager.guard_acquire_request() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.instance_id <> OLD.instance_id OR NEW.request <> OLD.request OR NEW.body_digest <> OLD.body_digest
        OR NEW.environment_id <> OLD.environment_id
        OR (OLD.lease_id IS NOT NULL AND NEW.lease_id IS DISTINCT FROM OLD.lease_id)
    THEN
        RAISE EXCEPTION 'manager: acquire requests keep their identity';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER acquire_request_identity BEFORE UPDATE ON manager.acquire_requests
    FOR EACH ROW EXECUTE FUNCTION manager.guard_acquire_request();
CREATE TRIGGER acquire_request_no_delete BEFORE DELETE ON manager.acquire_requests
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
