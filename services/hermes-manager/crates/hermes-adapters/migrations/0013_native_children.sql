-- Persist native Pod identity BEFORE removing its mandatory scheduling gate.
CREATE TABLE manager.native_children (
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    instance_id uuid NOT NULL REFERENCES manager.instances(id),
    pod_uid text NOT NULL CHECK(length(pod_uid) BETWEEN 1 AND 128),
    deployment_id uuid NOT NULL REFERENCES manager.owned_resources(id),
    provenance jsonb NOT NULL CHECK(jsonb_typeof(provenance)='object' AND octet_length(provenance::text)<=8192),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(installation_id,pod_uid)
);
CREATE INDEX native_children_instance ON manager.native_children(instance_id);
CREATE TRIGGER immutable_native_child BEFORE UPDATE OR DELETE ON manager.native_children
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.native_child_actions (
    operation_id uuid NOT NULL REFERENCES manager.operations(id),
    pod_uid text NOT NULL,
    action text NOT NULL CHECK(action IN ('start','stop')),
    input jsonb NOT NULL CHECK(jsonb_typeof(input)='object' AND octet_length(input::text)<=8192),
    input_digest text NOT NULL CHECK(input_digest ~ '^[0-9a-f]{64}$'),
    completed_at timestamptz,
    outcome text CHECK(outcome IN ('started','absent')),
    CHECK((completed_at IS NULL)=(outcome IS NULL)),
    CHECK(action<>'stop' OR outcome IS NULL OR outcome='absent'),
    PRIMARY KEY(operation_id,pod_uid,action,input_digest)
);
CREATE FUNCTION manager.guard_native_child_action() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' OR (to_jsonb(NEW)-'completed_at'-'outcome') IS DISTINCT FROM (to_jsonb(OLD)-'completed_at'-'outcome')
       OR (OLD.completed_at IS NOT NULL AND (NEW.completed_at IS DISTINCT FROM OLD.completed_at OR NEW.outcome IS DISTINCT FROM OLD.outcome)) THEN
        RAISE EXCEPTION 'native child action is immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER immutable_native_child_action BEFORE UPDATE OR DELETE ON manager.native_child_actions
    FOR EACH ROW EXECUTE FUNCTION manager.guard_native_child_action();
-- Bound pending recovery and retained-history queries independently of the
-- number of completed actions recorded for older Pods and operations.
CREATE INDEX native_child_actions_pending ON manager.native_child_actions(operation_id,action)
    WHERE completed_at IS NULL;
CREATE INDEX native_child_actions_pod ON manager.native_child_actions(pod_uid);
CREATE INDEX native_child_actions_absent ON manager.native_child_actions(pod_uid)
    WHERE outcome='absent';
