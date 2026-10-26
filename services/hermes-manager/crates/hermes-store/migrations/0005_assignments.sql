-- Assignments, their volumes and terminal backends, and their instances.
CREATE TABLE manager.assignments (
    id uuid PRIMARY KEY,
    identity_id uuid NOT NULL UNIQUE REFERENCES manager.identities (id),
    access text NOT NULL CHECK (access IN ('enabled', 'suspended', 'revoked')),
    template_kind text NOT NULL DEFAULT 'template' CHECK (template_kind = 'template'),
    template_id text NOT NULL,
    template_version bigint NOT NULL,
    terminal_mode text NOT NULL CHECK (terminal_mode IN ('per_session', 'per_profile_on_demand', 'per_profile_always_running')),
    default_backend text,
    admin_config jsonb NOT NULL DEFAULT '{}',
    secrets jsonb NOT NULL DEFAULT '[]',
    version bigint NOT NULL CHECK (version >= 1),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (template_kind, template_id, template_version) REFERENCES manager.catalog_versions (kind, id, version)
);

CREATE TABLE manager.assignment_volumes (
    assignment_id uuid NOT NULL REFERENCES manager.assignments (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (name ~ '^[a-z]([-a-z0-9]{0,61}[a-z0-9])?$'),
    source text NOT NULL,
    path text,
    size_gib integer CHECK (size_gib BETWEEN 1 AND 1048576),
    read_only boolean NOT NULL,
    PRIMARY KEY (assignment_id, name),
    CHECK ((path IS NULL) <> (size_gib IS NULL))
);
CREATE INDEX assignment_volumes_by_source ON manager.assignment_volumes (source) WHERE path IS NOT NULL;

-- Defence in depth for the domain rule: static paths of different
-- assignments on one source never overlap.
CREATE FUNCTION manager.guard_volume_overlap() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.path IS NOT NULL AND EXISTS (
        SELECT 1 FROM manager.assignment_volumes v
        WHERE v.source = NEW.source AND v.assignment_id <> NEW.assignment_id AND v.path IS NOT NULL
          AND (v.path = NEW.path OR left(v.path, length(NEW.path) + 1) = NEW.path || '/'
               OR left(NEW.path, length(v.path) + 1) = v.path || '/'))
    THEN
        RAISE EXCEPTION 'manager: volume paths of different owners overlap';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER volume_overlap BEFORE INSERT OR UPDATE ON manager.assignment_volumes
    FOR EACH ROW EXECUTE FUNCTION manager.guard_volume_overlap();

CREATE TABLE manager.assignment_backends (
    assignment_id uuid NOT NULL REFERENCES manager.assignments (id) ON DELETE CASCADE,
    backend_kind text NOT NULL DEFAULT 'terminal_backend' CHECK (backend_kind = 'terminal_backend'),
    backend_id text NOT NULL,
    backend_version bigint NOT NULL,
    PRIMARY KEY (assignment_id, backend_id),
    FOREIGN KEY (backend_kind, backend_id, backend_version) REFERENCES manager.catalog_versions (kind, id, version)
);

-- Desired side written by commands; status written only by the controller.
CREATE TABLE manager.instances (
    id uuid PRIMARY KEY,
    assignment_id uuid NOT NULL UNIQUE REFERENCES manager.assignments (id),
    namespace text NOT NULL UNIQUE,
    namespace_uid text,
    desired_generation bigint NOT NULL CHECK (desired_generation >= 1),
    execution_generation bigint NOT NULL CHECK (execution_generation >= 1),
    observed_generation bigint NOT NULL DEFAULT 0 CHECK (observed_generation >= 0),
    phase text NOT NULL DEFAULT 'provisioning'
        CHECK (phase IN ('provisioning', 'ready', 'stopping', 'retained', 'blocked')),
    blocked_reason text,
    conditions jsonb NOT NULL DEFAULT '[]',
    status_epoch bigint NOT NULL DEFAULT 0,
    observed_at timestamptz
);

CREATE FUNCTION manager.guard_instance() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.assignment_id <> OLD.assignment_id OR NEW.namespace <> OLD.namespace
        OR (OLD.namespace_uid IS NOT NULL AND NEW.namespace_uid IS DISTINCT FROM OLD.namespace_uid
            AND current_setting('manager.rebind', true) IS DISTINCT FROM 'on')
        OR NEW.desired_generation < OLD.desired_generation
        OR NEW.execution_generation < OLD.execution_generation
    THEN
        RAISE EXCEPTION 'manager: instance identity is immutable and generations only advance';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER instance_identity BEFORE UPDATE ON manager.instances
    FOR EACH ROW EXECUTE FUNCTION manager.guard_instance();
CREATE TRIGGER instance_no_delete BEFORE DELETE ON manager.instances
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TRIGGER assignment_no_delete BEFORE DELETE ON manager.assignments
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE FUNCTION manager.guard_assignment_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.identity_id <> OLD.identity_id OR NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'manager: assignments keep their identity and versions advance by one';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER assignment_identity BEFORE UPDATE ON manager.assignments
    FOR EACH ROW EXECUTE FUNCTION manager.guard_assignment_identity();
