CREATE TABLE manager.execution_clients (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    instance_id uuid NOT NULL UNIQUE REFERENCES manager.instances(id),
    material_id uuid NOT NULL UNIQUE REFERENCES manager.bootstrap_materials(id),
    token_hash bytea NOT NULL UNIQUE CHECK(octet_length(token_hash)=32),
    profile text COLLATE "C" NOT NULL CHECK(profile='default'),
    authorization_epoch bigint NOT NULL CHECK(authorization_epoch>0),
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(id,instance_id,installation_id)
);
CREATE FUNCTION manager.guard_execution_client() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'execution client provenance required' USING ERRCODE='23514'; END IF;
    IF TG_OP='UPDATE' AND ((to_jsonb(NEW)-'authorization_epoch'-'revoked_at') IS DISTINCT FROM (to_jsonb(OLD)-'authorization_epoch'-'revoked_at')
        OR NEW.authorization_epoch<OLD.authorization_epoch
        OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at)) THEN
        RAISE EXCEPTION 'immutable execution client identity' USING ERRCODE='23514';
    END IF;
    IF NOT EXISTS(SELECT 1 FROM manager.bootstrap_materials m JOIN manager.instances i ON i.id=m.instance_id
        JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.installations p ON p.id=a.installation_id
        WHERE m.id=NEW.material_id AND m.instance_id=NEW.instance_id AND m.installation_id=NEW.installation_id
          AND a.installation_id=NEW.installation_id AND m.purpose='native-bootstrap-v1'
          AND m.epoch<=NEW.authorization_epoch AND p.epoch=NEW.authorization_epoch) THEN
        RAISE EXCEPTION 'execution client scope mismatch' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER immutable_execution_client BEFORE INSERT OR UPDATE OR DELETE ON manager.execution_clients
    FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_client();
