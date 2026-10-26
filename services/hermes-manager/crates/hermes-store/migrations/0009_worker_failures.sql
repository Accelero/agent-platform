-- Why an environment's worker failed: recorded once, as the environment
-- starts to drain, and reported. No other image ever replaces it.
ALTER TABLE manager.environments ADD COLUMN failure text
    CHECK (failure IS NULL OR failure IN ('image_unavailable', 'start_failed', 'contract_failed'));

-- Domain rule: environment states only move forward, the Pod UID, SSH
-- address and failure are recorded once, and retired is final.
CREATE OR REPLACE FUNCTION manager.guard_environment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.instance_id <> OLD.instance_id OR NEW.backend_id <> OLD.backend_id
        OR NEW.backend_version <> OLD.backend_version OR NEW.execution_generation <> OLD.execution_generation
        OR NEW.mode <> OLD.mode OR NEW.key <> OLD.key OR NEW.invocation IS DISTINCT FROM OLD.invocation
        OR (OLD.pod_uid IS NOT NULL AND NEW.pod_uid IS DISTINCT FROM OLD.pod_uid)
        OR (OLD.ssh_host IS NOT NULL AND NEW.ssh_host IS DISTINCT FROM OLD.ssh_host)
        OR (OLD.failure IS NOT NULL AND NEW.failure IS DISTINCT FROM OLD.failure)
        OR (OLD.failure IS NULL AND NEW.failure IS NOT NULL
            AND NOT (OLD.state IN ('pending', 'starting') AND NEW.state = 'draining'))
        OR NOT (NEW.state = OLD.state
                OR (OLD.state = 'pending' AND NEW.state IN ('starting', 'draining'))
                OR (OLD.state = 'starting' AND NEW.state IN ('ready', 'draining'))
                OR (OLD.state = 'ready' AND NEW.state = 'draining')
                OR (OLD.state = 'draining' AND NEW.state = 'retired'))
        OR (OLD.state = 'retired' AND NEW IS DISTINCT FROM OLD)
    THEN
        RAISE EXCEPTION 'manager: environments only move forward';
    END IF;
    RETURN NEW;
END
$$;
