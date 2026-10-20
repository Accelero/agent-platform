-- Retained scale actions have complete immutable input and a reference to the
-- committed CLOSED namespace fence. No namespace, home or Deployment deletion.
CREATE TABLE manager.native_stop_inputs (
    operation_id uuid NOT NULL,
    step_key text NOT NULL,
    resource_id uuid NOT NULL REFERENCES manager.owned_resources(id),
    closed_fence_key text NOT NULL,
    input jsonb NOT NULL CHECK(jsonb_typeof(input)='object' AND octet_length(input::text)<=8192),
    input_digest text NOT NULL CHECK(input_digest ~ '^[0-9a-f]{64}$'),
    PRIMARY KEY(operation_id,step_key),
    FOREIGN KEY(operation_id,step_key) REFERENCES manager.resource_steps(operation_id,step_key),
    FOREIGN KEY(operation_id,closed_fence_key) REFERENCES manager.namespace_fence_inputs(operation_id,step_key)
);
CREATE TRIGGER immutable_native_stop_input BEFORE UPDATE OR DELETE ON manager.native_stop_inputs
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
