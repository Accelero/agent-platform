-- SSH identity is supplied only by the trusted controller, never by the native
-- caller. Historical SQL/legacy readiness fixtures have no pinned host key.
ALTER TABLE manager.execution_worker_ready_receipts ADD COLUMN host_key text
    CHECK(host_key IS NULL OR (length(host_key) BETWEEN 40 AND 1024 AND host_key LIKE 'ssh-ed25519 %'));

-- A NEW request may create a fresh worker after observed retirement. Historical
-- request/lease/environment identities and outcomes are never reused.
ALTER TABLE manager.execution_environments DROP CONSTRAINT execution_environments_instance_id_identity_digest_key;
CREATE UNIQUE INDEX execution_environment_active_identity
    ON manager.execution_environments(instance_id,identity_digest) WHERE state<>'Retired';
CREATE TABLE manager.execution_worker_stop_inputs (
    environment_id uuid NOT NULL, generation bigint NOT NULL, pod_uid text NOT NULL,
    resource_version text NOT NULL, spec_digest text NOT NULL, epoch bigint NOT NULL,
    PRIMARY KEY(environment_id,generation,resource_version),
    FOREIGN KEY(environment_id,generation) REFERENCES manager.execution_worker_intents(environment_id,generation)
);
CREATE TRIGGER immutable_execution_worker_stop BEFORE UPDATE OR DELETE ON manager.execution_worker_stop_inputs
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.execution_worker_absence (
    environment_id uuid NOT NULL, generation bigint NOT NULL, pod_uid text,
    verified_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(environment_id,generation),
    FOREIGN KEY(environment_id,generation) REFERENCES manager.execution_environments(id,generation)
);
CREATE TRIGGER immutable_execution_worker_absence BEFORE UPDATE OR DELETE ON manager.execution_worker_absence
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

-- A retirement claim may defer its own bounded observation without changing ownership.
-- Acquisition may touch last_used_at while a Ready observation claim is held,
-- without changing the observer's owner, deadline or fence. Ready lease changes
-- still require a new fence.
CREATE OR REPLACE FUNCTION manager.guard_execution_environment() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'environment history required' USING ERRCODE='23514'; END IF;
 IF (to_jsonb(NEW)-'state'-'pod_uid'-'resource_version'-'spec_digest'-'ssh_host'-'fence'-'lease_owner'-'lease_until'-'last_used_at'-'drain_deadline') IS DISTINCT FROM (to_jsonb(OLD)-'state'-'pod_uid'-'resource_version'-'spec_digest'-'ssh_host'-'fence'-'lease_owner'-'lease_until'-'last_used_at'-'drain_deadline')
 OR NEW.fence<OLD.fence OR NEW.fence>OLD.fence+1 OR (OLD.state IN ('Draining','Retired') AND NEW.state NOT IN ('Draining','Retired'))
 OR (OLD.state='Ready' AND NEW.state NOT IN ('Ready','Draining'))
 OR (NEW.lease_owner IS NOT NULL AND (NEW.lease_owner IS DISTINCT FROM OLD.lease_owner OR (OLD.state NOT IN ('Provisioning','Draining') AND NOT (OLD.state='Ready' AND NEW.state='Ready' AND NEW.lease_until IS NOT DISTINCT FROM OLD.lease_until))) AND NEW.fence<>OLD.fence+1)
 OR (OLD.drain_deadline IS NOT NULL AND NEW.drain_deadline IS DISTINCT FROM OLD.drain_deadline)
 OR (OLD.pod_uid IS NOT NULL AND (NEW.pod_uid IS DISTINCT FROM OLD.pod_uid OR NEW.spec_digest IS DISTINCT FROM OLD.spec_digest OR NEW.ssh_host IS DISTINCT FROM OLD.ssh_host)) THEN
 RAISE EXCEPTION 'immutable environment provenance' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END $$;
