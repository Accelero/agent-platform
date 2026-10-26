-- Installation identity, idempotency receipts and the audit log.
CREATE SCHEMA manager;

CREATE FUNCTION manager.reject_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'manager: % on % is not allowed', TG_OP, TG_TABLE_NAME;
END
$$;

-- One row. `id` and `issuer` never change; `cluster_uid` changes only through
-- the explicit `rebind-cluster` operator command.
CREATE TABLE manager.installation (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    id uuid NOT NULL,
    issuer text NOT NULL CHECK (issuer LIKE 'https://%'),
    cluster_uid text NOT NULL CHECK (length(cluster_uid) BETWEEN 1 AND 255),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE FUNCTION manager.guard_installation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.issuer <> OLD.issuer OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'manager: installation identity is immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER installation_identity BEFORE UPDATE ON manager.installation
    FOR EACH ROW EXECUTE FUNCTION manager.guard_installation();
CREATE TRIGGER installation_no_delete BEFORE DELETE ON manager.installation
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

-- Leader epoch of the controller. Status writes compare it.
CREATE TABLE manager.controller_epoch (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    epoch bigint NOT NULL CHECK (epoch >= 0)
);
INSERT INTO manager.controller_epoch (epoch) VALUES (0);

CREATE FUNCTION manager.guard_controller_epoch() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.epoch <= OLD.epoch THEN
        RAISE EXCEPTION 'manager: controller epoch must increase';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER controller_epoch_increases BEFORE UPDATE ON manager.controller_epoch
    FOR EACH ROW EXECUTE FUNCTION manager.guard_controller_epoch();

-- Idempotency: one row per actor, action and key. The operator uses empty
-- issuer and subject. A stored receipt never changes.
CREATE TABLE manager.command_receipts (
    actor_kind text NOT NULL CHECK (actor_kind IN ('administrator', 'operator')),
    actor_issuer text NOT NULL,
    actor_subject text NOT NULL,
    action text NOT NULL,
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    body_digest bytea NOT NULL CHECK (octet_length(body_digest) = 32),
    receipt jsonb,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (actor_kind, actor_issuer, actor_subject, action, idempotency_key)
);

CREATE FUNCTION manager.guard_command_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.receipt IS NOT NULL OR NEW.body_digest <> OLD.body_digest
        OR (NEW.actor_kind, NEW.actor_issuer, NEW.actor_subject, NEW.action, NEW.idempotency_key)
           IS DISTINCT FROM (OLD.actor_kind, OLD.actor_issuer, OLD.actor_subject, OLD.action, OLD.idempotency_key)
    THEN
        RAISE EXCEPTION 'manager: command receipts are immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER command_receipt_once BEFORE UPDATE ON manager.command_receipts
    FOR EACH ROW EXECUTE FUNCTION manager.guard_command_receipt();
CREATE TRIGGER command_receipt_no_delete BEFORE DELETE ON manager.command_receipts
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE TABLE manager.audit_events (
    id uuid PRIMARY KEY,
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    actor_kind text NOT NULL CHECK (actor_kind IN ('administrator', 'operator', 'controller')),
    actor_issuer text,
    actor_subject text,
    action text NOT NULL,
    target_kind text NOT NULL,
    target_id text NOT NULL,
    request_id uuid NOT NULL,
    change jsonb NOT NULL
);
CREATE INDEX audit_events_by_time ON manager.audit_events (occurred_at DESC, id);
CREATE INDEX audit_events_by_target ON manager.audit_events (target_kind, target_id, occurred_at DESC);
CREATE TRIGGER audit_events_immutable BEFORE UPDATE OR DELETE ON manager.audit_events
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
