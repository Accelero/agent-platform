-- Drain time is acknowledged desired state, not a process-local timer.
ALTER TABLE manager.instances
    ADD COLUMN drain_generation bigint,
    ADD COLUMN drain_epoch bigint,
    ADD COLUMN drain_started_at timestamptz,
    ADD COLUMN drain_deadline_at timestamptz,
    ADD COLUMN drain_completed_at timestamptz,
    ADD CONSTRAINT drain_barrier_shape CHECK (
        (drain_generation IS NULL AND drain_epoch IS NULL AND drain_started_at IS NULL
         AND drain_deadline_at IS NULL AND drain_completed_at IS NULL)
        OR (drain_generation IS NOT NULL AND drain_epoch IS NOT NULL
         AND drain_started_at IS NOT NULL AND drain_deadline_at IS NOT NULL
         AND drain_generation>0 AND drain_generation<=desired_generation AND drain_epoch>0
         AND drain_deadline_at=drain_started_at+interval '5 minutes'
         AND (drain_completed_at IS NULL OR drain_completed_at>=drain_started_at))
    );

-- Earlier development schemas could acknowledge a disable without a barrier.
-- They never proved drain completion. Preserve the earliest recorded deadline
-- conservatively, including rapid re-enable, instead of granting a new window.
WITH original AS (
    SELECT DISTINCT ON (instance_id) instance_id,generation,epoch,created_at
    FROM manager.operations WHERE input->>'access' IN ('suspended','revoked')
    ORDER BY instance_id,generation
)
UPDATE manager.instances i SET drain_generation=o.generation,drain_epoch=o.epoch,
    drain_started_at=o.created_at,drain_deadline_at=o.created_at+interval '5 minutes'
FROM original o WHERE o.instance_id=i.id;

CREATE FUNCTION manager.guard_drain_barrier() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.drain_started_at IS DISTINCT FROM OLD.drain_started_at THEN
        IF NEW.drain_started_at IS NULL OR NEW.drain_generation<>NEW.desired_generation
            OR NOT EXISTS(SELECT 1 FROM manager.assignments a JOIN manager.installations p
                ON p.id=a.installation_id WHERE a.id=NEW.assignment_id
                AND a.access IN ('suspended','revoked') AND p.epoch=NEW.drain_epoch) THEN
            RAISE EXCEPTION 'drain requires current disabled generation' USING ERRCODE='23514';
        END IF;
    END IF;
    IF OLD.drain_started_at IS NOT NULL THEN
        IF NEW.drain_started_at IS NULL THEN
            RAISE EXCEPTION 'drain provenance required' USING ERRCODE='23514';
        ELSIF NEW.drain_started_at<>OLD.drain_started_at THEN
            IF OLD.drain_completed_at IS NULL OR NEW.drain_generation<=OLD.drain_generation
                OR NEW.drain_epoch<OLD.drain_epoch OR NEW.drain_completed_at IS NOT NULL THEN
                RAISE EXCEPTION 'active drain deadline is immutable' USING ERRCODE='23514';
            END IF;
        ELSIF NEW.drain_deadline_at IS DISTINCT FROM OLD.drain_deadline_at
            OR NEW.drain_generation IS DISTINCT FROM OLD.drain_generation
            OR NEW.drain_epoch IS DISTINCT FROM OLD.drain_epoch
            OR (OLD.drain_completed_at IS NOT NULL
                AND NEW.drain_completed_at IS DISTINCT FROM OLD.drain_completed_at) THEN
            RAISE EXCEPTION 'immutable drain provenance' USING ERRCODE='23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_drain_barrier BEFORE UPDATE ON manager.instances
    FOR EACH ROW EXECUTE FUNCTION manager.guard_drain_barrier();
