-- Definitions are software/policy, not arbitrary Pod specs or personal seeds.
-- Old reference-only rows cannot commission until a matching reviewed definition
-- is registered explicitly. Never infer defaults for an existing assignment.
CREATE TABLE manager.template_definitions (
    template_id text COLLATE "C" NOT NULL,
    template_version bigint NOT NULL,
    definition jsonb NOT NULL CHECK(jsonb_typeof(definition)='object' AND octet_length(definition::text)<=16384),
    PRIMARY KEY(template_id,template_version),
    FOREIGN KEY(template_id,template_version) REFERENCES manager.instance_templates(id,version)
);
CREATE TRIGGER immutable_template_definition BEFORE UPDATE OR DELETE ON manager.template_definitions
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

ALTER TABLE manager.operations ADD COLUMN last_condition jsonb
    CHECK(last_condition IS NULL OR jsonb_typeof(last_condition)='object');

CREATE FUNCTION manager.guard_operation_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'operation provenance required' USING ERRCODE='23514'; END IF;
    IF NEW.id<>OLD.id OR NEW.installation_id<>OLD.installation_id OR NEW.instance_id<>OLD.instance_id
        OR NEW.kind<>OLD.kind OR NEW.generation<>OLD.generation OR NEW.epoch<>OLD.epoch
        OR NEW.input<>OLD.input OR NEW.created_at<>OLD.created_at THEN
        RAISE EXCEPTION 'immutable operation identity' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_operation_identity BEFORE UPDATE OR DELETE ON manager.operations
    FOR EACH ROW EXECUTE FUNCTION manager.guard_operation_identity();
