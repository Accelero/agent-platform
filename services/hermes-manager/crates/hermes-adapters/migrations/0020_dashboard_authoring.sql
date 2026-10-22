-- Dashboard authoring is separate from live infrastructure desired state.
CREATE TABLE manager.dashboard_template_drafts (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    template_id text COLLATE "C" NOT NULL CHECK(template_id ~ '^[a-z][a-z0-9-]{0,62}$'),
    definition jsonb NOT NULL CHECK(jsonb_typeof(definition)='object' AND octet_length(definition::text)<=16384),
    version bigint NOT NULL DEFAULT 1 CHECK(version>0),
    published_version bigint,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX dashboard_draft_template ON manager.dashboard_template_drafts(installation_id,template_id,id);
CREATE FUNCTION manager.guard_published_draft() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.published_version IS NOT NULL THEN RAISE EXCEPTION 'published draft is immutable'; END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER immutable_published_draft BEFORE UPDATE OR DELETE ON manager.dashboard_template_drafts
    FOR EACH ROW EXECUTE FUNCTION manager.guard_published_draft();
CREATE TABLE manager.dashboard_commands (
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    actor_issuer text NOT NULL,
    actor_subject text NOT NULL,
    idempotency_key text NOT NULL CHECK(octet_length(idempotency_key) BETWEEN 1 AND 128),
    body_digest bytea NOT NULL,
    result jsonb,
    PRIMARY KEY(installation_id,actor_issuer,actor_subject,idempotency_key)
);
CREATE TABLE manager.dashboard_operations (
    id uuid PRIMARY KEY,
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    kind text NOT NULL,
    state text NOT NULL CHECK(state IN ('Pending','Succeeded','Failed')),
    result jsonb NOT NULL DEFAULT '{}'::jsonb
);
ALTER TABLE manager.audit_events ALTER COLUMN assignment_id DROP NOT NULL;
ALTER TABLE manager.audit_events ALTER COLUMN operation_id DROP NOT NULL;
ALTER TABLE manager.audit_events ADD COLUMN entity_type text;
ALTER TABLE manager.audit_events ADD COLUMN entity_id text;
ALTER TABLE manager.audit_events ADD COLUMN entity_operation_id uuid REFERENCES manager.dashboard_operations(id);

-- Serialize source ownership against infrastructure edits without sharing live rows.
CREATE FUNCTION manager.guard_infrastructure_template_authority() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.id,928451));
    IF EXISTS(SELECT 1 FROM manager.dashboard_template_drafts WHERE template_id=NEW.id) THEN
        RAISE EXCEPTION 'template source conflict';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER protect_manager_template_source BEFORE INSERT OR UPDATE ON manager.infrastructure_templates
    FOR EACH ROW EXECUTE FUNCTION manager.guard_infrastructure_template_authority();
