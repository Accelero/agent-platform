-- Immutable reviewed software policy and exact owner-local references. No
-- credential values, raw manifests, host paths or mutable static YAML here.
CREATE TABLE manager.terminal_runtime_versions (
    id text COLLATE "C" NOT NULL CHECK(id ~ '^[a-z][a-z0-9-]{0,62}$'),
    version bigint NOT NULL CHECK(version>0),
    schema_version integer NOT NULL CHECK(schema_version=1),
    content_digest text NOT NULL CHECK(content_digest ~ '^[0-9a-f]{64}$'),
    definition jsonb NOT NULL CHECK(jsonb_typeof(definition)='object' AND octet_length(definition::text)<=16384),
    PRIMARY KEY(id,version)
);
CREATE TRIGGER immutable_terminal_runtime BEFORE UPDATE OR DELETE ON manager.terminal_runtime_versions
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

ALTER TABLE manager.instances ADD UNIQUE(id,namespace,namespace_uid);
ALTER TABLE manager.instances ADD UNIQUE(assignment_id,id);

CREATE TABLE manager.storage_binding_owners (
    id uuid PRIMARY KEY,
    instance_id uuid NOT NULL REFERENCES manager.instances(id),
    UNIQUE(id,instance_id)
);
CREATE TRIGGER immutable_storage_owner BEFORE UPDATE OR DELETE ON manager.storage_binding_owners
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.storage_binding_versions (
    id uuid NOT NULL,
    version bigint NOT NULL CHECK(version>0),
    instance_id uuid NOT NULL,
    namespace text COLLATE "C" NOT NULL,
    namespace_uid text NOT NULL,
    pvc_name text COLLATE "C" NOT NULL,
    pvc_uid text NOT NULL,
    definition jsonb NOT NULL CHECK(jsonb_typeof(definition)='object' AND octet_length(definition::text)<=16384),
    PRIMARY KEY(id,version),
    FOREIGN KEY(id,instance_id) REFERENCES manager.storage_binding_owners(id,instance_id),
    FOREIGN KEY(instance_id,namespace,namespace_uid) REFERENCES manager.instances(id,namespace,namespace_uid)
);
CREATE TRIGGER immutable_storage_version BEFORE UPDATE OR DELETE ON manager.storage_binding_versions
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE TABLE manager.execution_pull_secrets (
    instance_id uuid NOT NULL,
    namespace text COLLATE "C" NOT NULL,
    namespace_uid text NOT NULL,
    name text COLLATE "C" NOT NULL,
    uid text NOT NULL,
    PRIMARY KEY(instance_id,name),
    FOREIGN KEY(instance_id,namespace,namespace_uid) REFERENCES manager.instances(id,namespace,namespace_uid)
);
CREATE TRIGGER immutable_execution_pull_secret BEFORE UPDATE OR DELETE ON manager.execution_pull_secrets
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE TABLE manager.execution_policy_versions (
    instance_id uuid NOT NULL REFERENCES manager.instances(id),
    generation bigint NOT NULL CHECK(generation>0),
    content_digest text NOT NULL CHECK(content_digest ~ '^[0-9a-f]{64}$'),
    policy jsonb NOT NULL CHECK(jsonb_typeof(policy)='object' AND octet_length(policy::text)<=262144),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(instance_id,generation)
);
CREATE TRIGGER immutable_execution_policy BEFORE UPDATE OR DELETE ON manager.execution_policy_versions
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.execution_policy_status (
    instance_id uuid PRIMARY KEY REFERENCES manager.instances(id),
    desired_generation bigint NOT NULL CHECK(desired_generation>0),
    -- This foundation deliberately cannot claim applied. A later migration
    -- must supply the actual environment drain/readiness activation contract.
    applied_generation bigint NOT NULL DEFAULT 0 CHECK(applied_generation=0),
    state text NOT NULL CHECK(state IN ('Pending','Reconciling','Waiting')),
    epoch bigint NOT NULL CHECK(epoch>0),
    fence bigint NOT NULL DEFAULT 0 CHECK(fence>=0),
    lease_owner uuid,
    lease_until timestamptz,
    next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    condition text CHECK(condition IN ('admission_pending','storage_pending','drain_pending','dependency_unavailable')),
    CHECK((lease_owner IS NULL)=(lease_until IS NULL)),
    CHECK((state='Reconciling')=(lease_owner IS NOT NULL)),
    FOREIGN KEY(instance_id,desired_generation) REFERENCES manager.execution_policy_versions(instance_id,generation)
);
CREATE INDEX execution_policy_due ON manager.execution_policy_status(next_attempt_at);

CREATE FUNCTION manager.guard_execution_policy_status() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'execution policy provenance required' USING ERRCODE='23514'; END IF;
    IF NEW.instance_id<>OLD.instance_id OR NEW.created_at<>OLD.created_at
        OR NEW.desired_generation<OLD.desired_generation OR NEW.desired_generation>OLD.desired_generation+1
        OR NEW.epoch<OLD.epoch OR NEW.fence<OLD.fence OR NEW.fence>OLD.fence+1 THEN
        RAISE EXCEPTION 'immutable execution policy provenance' USING ERRCODE='23514';
    END IF;
    IF NEW.desired_generation<>OLD.desired_generation OR NEW.epoch<>OLD.epoch THEN
        IF NEW.fence<>OLD.fence+1 OR NEW.state<>'Pending' OR NEW.lease_owner IS NOT NULL THEN
            RAISE EXCEPTION 'policy changes must fence prior work' USING ERRCODE='23514';
        END IF;
    END IF;
    IF NEW.lease_owner IS NOT NULL AND (NEW.lease_owner IS DISTINCT FROM OLD.lease_owner OR OLD.state<>'Reconciling')
        AND NEW.fence<>OLD.fence+1 THEN
        RAISE EXCEPTION 'new policy claim requires a new fence' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_execution_policy_status BEFORE UPDATE OR DELETE ON manager.execution_policy_status
    FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_policy_status();

-- Immutable audit/idempotency acknowledgement records with redacted actor and
-- target metadata. Policy changes also enqueue fenced per-instance work above.
CREATE TABLE manager.execution_configuration_events (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    actor_issuer text COLLATE "C" NOT NULL,
    actor_subject text COLLATE "C" NOT NULL,
    action text NOT NULL,
    request_id uuid NOT NULL,
    change jsonb NOT NULL CHECK(jsonb_typeof(change)='object' AND octet_length(change::text)<=4096),
    result jsonb NOT NULL CHECK(jsonb_typeof(result)='object'),
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TRIGGER immutable_execution_configuration_event BEFORE UPDATE OR DELETE ON manager.execution_configuration_events
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
