CREATE TABLE manager.execution_policy_renderings (
 instance_id uuid NOT NULL,generation bigint NOT NULL,epoch bigint NOT NULL CHECK(epoch>0),
 policy_digest text NOT NULL CHECK(policy_digest ~ '^[0-9a-f]{64}$'),
 rendering_digest text NOT NULL CHECK(rendering_digest ~ '^[0-9a-f]{64}$'),
 worker jsonb NOT NULL CHECK(jsonb_typeof(worker)='object' AND octet_length(worker::text)<=131072),
 PRIMARY KEY(instance_id,generation,epoch),
 FOREIGN KEY(instance_id,generation) REFERENCES manager.execution_policy_versions(instance_id,generation)
);
CREATE TRIGGER immutable_policy_rendering BEFORE UPDATE OR DELETE ON manager.execution_policy_renderings FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.execution_policy_admission_inputs (
 instance_id uuid NOT NULL,generation bigint NOT NULL,epoch bigint NOT NULL,
 input_digest text NOT NULL CHECK(input_digest ~ '^[0-9a-f]{64}$'),
 input jsonb NOT NULL CHECK(jsonb_typeof(input)='object' AND octet_length(input::text)<=8192),
 fence bigint NOT NULL CHECK(fence>0),result jsonb,verified_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(instance_id,generation,epoch,input_digest),
 FOREIGN KEY(instance_id,generation,epoch) REFERENCES manager.execution_policy_renderings(instance_id,generation,epoch),
 CHECK((result IS NULL)=(verified_at IS NULL))
);
CREATE FUNCTION manager.guard_policy_admission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'admission action history required' USING ERRCODE='23514'; END IF;
 IF (to_jsonb(NEW)-'fence'-'result'-'verified_at') IS DISTINCT FROM (to_jsonb(OLD)-'fence'-'result'-'verified_at')
 OR NEW.fence<OLD.fence OR (OLD.result IS NOT NULL AND (NEW.result IS DISTINCT FROM OLD.result OR NEW.verified_at IS DISTINCT FROM OLD.verified_at)) THEN
 RAISE EXCEPTION 'immutable admission action input/result' USING ERRCODE='23514'; END IF; RETURN NEW;
END $$;
CREATE TRIGGER immutable_policy_admission BEFORE UPDATE OR DELETE ON manager.execution_policy_admission_inputs FOR EACH ROW EXECUTE FUNCTION manager.guard_policy_admission();
CREATE INDEX policy_admission_pending ON manager.execution_policy_admission_inputs(instance_id,generation,epoch) WHERE verified_at IS NULL;
ALTER TABLE manager.execution_policy_status DROP CONSTRAINT execution_policy_status_applied_generation_check;
ALTER TABLE manager.execution_policy_status ADD CONSTRAINT execution_policy_status_applied_generation_check CHECK(applied_generation>=0 AND applied_generation<=desired_generation);
ALTER TABLE manager.execution_policy_status DROP CONSTRAINT execution_policy_status_state_check;
ALTER TABLE manager.execution_policy_status ADD CONSTRAINT execution_policy_status_state_check CHECK(state IN ('Pending','Reconciling','Waiting','Applied'));
-- The original guard still enforces desired generation and claim fencing. This
-- additional guard prevents arbitrary applied flags from bypassing readback.
CREATE FUNCTION manager.guard_applied_policy() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF NEW.applied_generation<>OLD.applied_generation THEN
  IF NEW.applied_generation=0 AND NEW.epoch>OLD.epoch THEN RETURN NEW; END IF;
  IF NEW.applied_generation<OLD.applied_generation OR NEW.state<>'Applied'
   OR NEW.applied_generation<>NEW.desired_generation
   OR NOT EXISTS(SELECT 1 FROM manager.execution_effective_policy_receipts r
      JOIN manager.execution_policy_admission_inputs p ON p.instance_id=r.instance_id AND p.generation=r.generation AND p.epoch=r.epoch
      WHERE r.instance_id=NEW.instance_id AND r.generation=NEW.applied_generation AND r.epoch=NEW.epoch AND p.verified_at IS NOT NULL AND p.fence=NEW.fence AND p.input->>'policyDigest'=r.content_digest)
   OR EXISTS(SELECT 1 FROM manager.execution_policy_admission_inputs p WHERE p.instance_id=NEW.instance_id AND p.generation=NEW.desired_generation AND p.epoch=NEW.epoch AND p.verified_at IS NULL)
   OR EXISTS(SELECT 1 FROM manager.execution_environments e WHERE e.instance_id=NEW.instance_id AND (e.policy_generation<>NEW.desired_generation OR e.epoch<>NEW.epoch) AND e.state<>'Retired') THEN
   RAISE EXCEPTION 'effective policy requires verified admission and prior-generation absence' USING ERRCODE='23514';
  END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER verified_applied_policy BEFORE UPDATE ON manager.execution_policy_status FOR EACH ROW EXECUTE FUNCTION manager.guard_applied_policy();
