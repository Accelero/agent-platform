-- Create-once material is ciphertext only; no raw bootstrap values enter audit.
CREATE TABLE manager.bootstrap_materials (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    instance_id uuid NOT NULL REFERENCES manager.instances(id),
    epoch bigint NOT NULL CHECK(epoch>0),
    purpose text NOT NULL CHECK(purpose ~ '^[a-z][a-z0-9-]{0,62}$'),
    key_id text NOT NULL,
    nonce bytea NOT NULL CHECK(octet_length(nonce)=24),
    ciphertext bytea NOT NULL CHECK(octet_length(ciphertext) BETWEEN 16 AND 65536),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(instance_id,purpose)
);
CREATE TRIGGER immutable_bootstrap_material BEFORE UPDATE OR DELETE ON manager.bootstrap_materials
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE TABLE manager.owned_resources (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    instance_id uuid NOT NULL REFERENCES manager.instances(id),
    kind text NOT NULL CHECK(kind IN ('Namespace','RoleBinding','ServiceAccount','PersistentVolumeClaim','Secret','ConfigMap','NetworkPolicy','Service','Deployment','Pod')),
    namespace text NOT NULL DEFAULT '',
    name text NOT NULL CHECK(name ~ '^[a-z][a-z0-9-]{0,62}$'),
    creation jsonb NOT NULL CHECK(jsonb_typeof(creation)='object'),
    uid text CHECK(length(uid) BETWEEN 1 AND 255),
    resource_version text CHECK(length(resource_version) BETWEEN 1 AND 255),
    observed_digest text CHECK(observed_digest ~ '^[0-9a-f]{64}$'),
    state text NOT NULL DEFAULT 'Reserved' CHECK(state IN ('Reserved','Bound','Retiring','Deleted')),
    encrypted_material_ref uuid REFERENCES manager.bootstrap_materials(id),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK((kind='Namespace')=(namespace='')),
    CHECK((state='Reserved')=(uid IS NULL)),
    CHECK((uid IS NULL)=(resource_version IS NULL)),
    CHECK((state='Reserved')=(observed_digest IS NULL)),
    UNIQUE(installation_id,kind,namespace,name)
);
CREATE TABLE manager.resource_steps (
    operation_id uuid NOT NULL,
    step_key text NOT NULL,
    resource_id uuid NOT NULL REFERENCES manager.owned_resources(id),
    PRIMARY KEY(operation_id,step_key),
    FOREIGN KEY(operation_id,step_key) REFERENCES manager.operation_steps(operation_id,step_key)
);
CREATE TRIGGER immutable_resource_step BEFORE UPDATE OR DELETE ON manager.resource_steps
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE FUNCTION manager.guard_resource() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'owned resource tombstone required' USING ERRCODE='23514'; END IF;
    IF NEW.id<>OLD.id OR NEW.installation_id<>OLD.installation_id OR NEW.instance_id<>OLD.instance_id
        OR NEW.kind<>OLD.kind OR NEW.namespace<>OLD.namespace OR NEW.name<>OLD.name OR NEW.creation<>OLD.creation
        OR NEW.encrypted_material_ref IS DISTINCT FROM OLD.encrypted_material_ref
        OR (OLD.uid IS NOT NULL AND NEW.uid IS DISTINCT FROM OLD.uid)
        OR (OLD.state='Deleted' AND NEW.state<>'Deleted')
        OR (OLD.state='Retiring' AND NEW.state NOT IN ('Retiring','Deleted'))
        OR (OLD.state='Bound' AND NEW.state NOT IN ('Bound','Retiring'))
        OR (OLD.state='Reserved' AND NEW.state NOT IN ('Reserved','Bound')) THEN
        RAISE EXCEPTION 'immutable resource ownership' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_resource_ownership BEFORE UPDATE OR DELETE ON manager.owned_resources
    FOR EACH ROW EXECUTE FUNCTION manager.guard_resource();

CREATE FUNCTION manager.guard_instance_owner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'instance lifecycle required' USING ERRCODE='23514'; END IF;
    IF NEW.id<>OLD.id OR NEW.assignment_id<>OLD.assignment_id OR NEW.namespace<>OLD.namespace
        OR (OLD.namespace_uid IS NOT NULL AND NEW.namespace_uid IS DISTINCT FROM OLD.namespace_uid) THEN
        RAISE EXCEPTION 'immutable instance ownership' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_instance_owner BEFORE UPDATE OR DELETE ON manager.instances
    FOR EACH ROW EXECUTE FUNCTION manager.guard_instance_owner();
