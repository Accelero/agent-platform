-- Admission changes never overwrite immutable Namespace creation provenance.
CREATE TABLE manager.namespace_fence_inputs (
    operation_id uuid NOT NULL,
    step_key text NOT NULL,
    resource_id uuid NOT NULL REFERENCES manager.owned_resources(id),
    input jsonb NOT NULL CHECK(jsonb_typeof(input)='object' AND octet_length(input::text)<=4096),
    input_digest text NOT NULL CHECK(input_digest ~ '^[0-9a-f]{64}$'),
    PRIMARY KEY(operation_id,step_key),
    FOREIGN KEY(operation_id,step_key) REFERENCES manager.resource_steps(operation_id,step_key)
);
CREATE TRIGGER immutable_namespace_fence_input BEFORE UPDATE OR DELETE ON manager.namespace_fence_inputs
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
