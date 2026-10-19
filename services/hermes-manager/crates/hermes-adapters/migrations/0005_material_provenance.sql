-- The reserved table had no credential writer before this migration. Refuse
-- unexplained partial records rather than infer missing creation provenance.
ALTER TABLE manager.bootstrap_materials
    ADD COLUMN creation_operation uuid NOT NULL REFERENCES manager.operations(id),
    ADD COLUMN creation_generation bigint NOT NULL CHECK(creation_generation>0),
    ADD CONSTRAINT material_key_name CHECK(key_id ~ '^[a-z][a-z0-9-]{0,62}$');

CREATE FUNCTION manager.guard_material_creation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM manager.operations o
        JOIN manager.instances i ON i.id=o.instance_id
        JOIN manager.assignments a ON a.id=i.assignment_id
        WHERE o.id=NEW.creation_operation AND o.instance_id=NEW.instance_id
          AND o.installation_id=NEW.installation_id AND a.installation_id=NEW.installation_id
          AND o.epoch=NEW.epoch AND o.generation=NEW.creation_generation
    ) THEN
        RAISE EXCEPTION 'material provenance mismatch' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER material_creation_provenance BEFORE INSERT ON manager.bootstrap_materials
    FOR EACH ROW EXECUTE FUNCTION manager.guard_material_creation();
