ALTER TABLE manager.assignments ADD COLUMN dashboard_managed boolean NOT NULL DEFAULT false;
ALTER TABLE manager.assignments ADD COLUMN dashboard_definition jsonb;
ALTER TABLE manager.assignments ADD CONSTRAINT single_assignment_source CHECK(NOT(infrastructure_managed AND dashboard_managed));
CREATE TABLE manager.dashboard_template_batches (
    id uuid PRIMARY KEY REFERENCES manager.dashboard_operations(id),
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    template_id text NOT NULL,
    template_version bigint NOT NULL,
    actor_issuer text NOT NULL,
    actor_subject text NOT NULL
);
CREATE TABLE manager.dashboard_template_batch_items (
    batch_id uuid NOT NULL REFERENCES manager.dashboard_template_batches(id),
    assignment_id uuid NOT NULL REFERENCES manager.assignments(id),
    expected_version bigint NOT NULL CHECK(expected_version>0),
    state text NOT NULL DEFAULT 'Pending' CHECK(state IN ('Pending','Accepted','Failed')),
    operation_id uuid REFERENCES manager.operations(id),
    error_code text,
    PRIMARY KEY(batch_id,assignment_id)
);

-- Explicit dashboard rollouts may drain an enabled native instance only after
-- the fenced commissioning operation has been recorded in the same transaction.
CREATE OR REPLACE FUNCTION manager.guard_drain_barrier() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.drain_started_at IS DISTINCT FROM OLD.drain_started_at THEN
        IF NEW.drain_started_at IS NULL OR NEW.drain_generation<>NEW.desired_generation
            OR NOT EXISTS(SELECT 1 FROM manager.assignments a JOIN manager.installations p
                ON p.id=a.installation_id WHERE a.id=NEW.assignment_id
                AND (a.access IN ('suspended','revoked') OR (a.dashboard_managed AND EXISTS(
                    SELECT 1 FROM manager.operations o WHERE o.instance_id=NEW.id
                    AND o.generation=NEW.desired_generation AND o.epoch=NEW.drain_epoch
                    AND o.kind='commission' AND o.state='Pending')))
                AND p.epoch=NEW.drain_epoch) THEN
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
