CREATE SCHEMA manager;

-- All control records are ordinary WAL-logged tables. Session stores will use
-- separate schemas/roles and are deliberately not assignment writer tables.
CREATE TABLE manager.installations (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    id uuid NOT NULL UNIQUE,
    issuer text COLLATE "C" NOT NULL CHECK (length(issuer) BETWEEN 1 AND 2048),
    schema_version bigint NOT NULL CHECK (schema_version = 1),
    template_schema_version bigint NOT NULL CHECK (template_schema_version = 1),
    epoch bigint NOT NULL DEFAULT 1 CHECK (epoch > 0),
    mode text NOT NULL CHECK (mode IN ('active', 'maintenance', 'restore')),
    cluster_identity text NOT NULL CHECK (length(cluster_identity) BETWEEN 1 AND 255),
    fresh_inventory_digest text NOT NULL CHECK (fresh_inventory_digest ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE manager.identities (
    id uuid PRIMARY KEY,
    issuer text COLLATE "C" NOT NULL CHECK (length(issuer) BETWEEN 1 AND 2048),
    subject text COLLATE "C" NOT NULL CHECK (octet_length(subject) BETWEEN 1 AND 255),
    UNIQUE (issuer, subject)
);

CREATE TABLE manager.instance_templates (
    id text COLLATE "C" NOT NULL CHECK (id ~ '^[a-z][a-z0-9-]{0,62}$'),
    version bigint NOT NULL CHECK (version > 0),
    schema_version bigint NOT NULL CHECK (schema_version = 1),
    image text NOT NULL CHECK (image ~ '^.+@sha256:[0-9a-f]{64}$'),
    content_digest text NOT NULL CHECK (content_digest ~ '^[0-9a-f]{64}$'),
    approved boolean NOT NULL DEFAULT false,
    PRIMARY KEY (id, version)
);

CREATE TABLE manager.assignments (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    identity_id uuid NOT NULL REFERENCES manager.identities(id),
    access text NOT NULL CHECK (access IN ('enabled', 'suspended', 'revoked')),
    template_id text COLLATE "C" NOT NULL,
    template_version bigint NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (installation_id, identity_id),
    FOREIGN KEY (template_id, template_version) REFERENCES manager.instance_templates(id, version)
);

CREATE TABLE manager.instances (
    id uuid PRIMARY KEY,
    assignment_id uuid NOT NULL UNIQUE REFERENCES manager.assignments(id),
    namespace text NOT NULL UNIQUE CHECK (length(namespace) <= 63 AND namespace ~ '^hi-[0-9a-f]{24}-[0-9a-f]{32}$'),
    namespace_uid text,
    desired_generation bigint NOT NULL CHECK (desired_generation > 0),
    observed_generation bigint NOT NULL DEFAULT 0 CHECK (observed_generation >= 0 AND observed_generation <= desired_generation),
    state text NOT NULL DEFAULT 'Pending' CHECK (state IN (
        'Pending', 'Provisioning', 'Ready', 'Blocked', 'Suspending', 'Suspended',
        'Retained', 'Deleting', 'Deleted', 'Error'
    )),
    conditions jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(conditions) = 'object'),
    route_generation bigint NOT NULL DEFAULT 0 CHECK (route_generation >= 0)
);

CREATE TABLE manager.operations (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    instance_id uuid NOT NULL REFERENCES manager.instances(id),
    kind text NOT NULL CHECK (kind IN ('commission', 'suspend', 'revoke', 'retry')),
    generation bigint NOT NULL CHECK (generation > 0),
    epoch bigint NOT NULL CHECK (epoch > 0),
    -- Pin immutable configuration, rather than reread a mutable request on retry.
    input jsonb NOT NULL CHECK (jsonb_typeof(input) = 'object'),
    state text NOT NULL DEFAULT 'Pending' CHECK (state IN (
        'Pending', 'Running', 'Waiting', 'RetryAt', 'Succeeded', 'Blocked', 'Failed', 'Superseded'
    )),
    fence bigint NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_owner uuid,
    lease_until timestamptz,
    attempt integer NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    max_attempts integer NOT NULL DEFAULT 20 CHECK (max_attempts BETWEEN 1 AND 100),
    next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((lease_owner IS NULL) = (lease_until IS NULL)),
    CHECK ((state = 'Running') = (lease_owner IS NOT NULL)),
    UNIQUE (instance_id, generation)
);
CREATE INDEX operations_due ON manager.operations (next_attempt_at, created_at)
    WHERE state IN ('Pending', 'Running', 'Waiting', 'RetryAt');

CREATE TABLE manager.mutation_requests (
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    actor_issuer text COLLATE "C" NOT NULL,
    actor_subject text COLLATE "C" NOT NULL,
    scope text NOT NULL,
    idempotency_key text COLLATE "C" NOT NULL CHECK (octet_length(idempotency_key) BETWEEN 1 AND 128),
    body_digest bytea NOT NULL CHECK (octet_length(body_digest) = 32),
    result jsonb,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (installation_id, actor_issuer, actor_subject, scope, idempotency_key)
);

CREATE TABLE manager.audit_events (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    actor_issuer text COLLATE "C" NOT NULL,
    actor_subject text COLLATE "C" NOT NULL,
    action text NOT NULL,
    assignment_id uuid NOT NULL REFERENCES manager.assignments(id),
    operation_id uuid NOT NULL REFERENCES manager.operations(id),
    request_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    change jsonb NOT NULL CHECK (jsonb_typeof(change) = 'object'),
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE manager.operation_steps (
    operation_id uuid NOT NULL REFERENCES manager.operations(id),
    step_key text NOT NULL CHECK (step_key ~ '^[a-z][a-z0-9-]{0,62}$'),
    intent jsonb NOT NULL CHECK (jsonb_typeof(intent) = 'object'),
    result jsonb NOT NULL DEFAULT '{"status":"unknown"}'::jsonb,
    fence bigint NOT NULL CHECK (fence > 0),
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    verified_at timestamptz,
    PRIMARY KEY (operation_id, step_key)
);

-- Direct mistakes in future adapters must not mutate ownership or rewrite the
-- input of an already committed external action.
CREATE FUNCTION manager.reject_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'immutable manager record' USING ERRCODE = '23514';
END;
$$;
CREATE TRIGGER immutable_identity BEFORE UPDATE OR DELETE ON manager.identities
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TRIGGER immutable_audit BEFORE UPDATE OR DELETE ON manager.audit_events
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TRIGGER immutable_template BEFORE UPDATE OR DELETE ON manager.instance_templates
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE FUNCTION manager.guard_assignment_owner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.installation_id <> OLD.installation_id OR NEW.identity_id <> OLD.identity_id THEN
        RAISE EXCEPTION 'immutable assignment ownership' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_assignment_owner BEFORE UPDATE ON manager.assignments
    FOR EACH ROW EXECUTE FUNCTION manager.guard_assignment_owner();

CREATE FUNCTION manager.guard_step_intent() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.operation_id <> OLD.operation_id OR NEW.step_key <> OLD.step_key OR NEW.intent <> OLD.intent THEN
        RAISE EXCEPTION 'immutable operation intent' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_step_intent BEFORE UPDATE ON manager.operation_steps
    FOR EACH ROW EXECUTE FUNCTION manager.guard_step_intent();
