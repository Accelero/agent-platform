-- Infrastructure writes desired state; controller-owned history is never deleted.
CREATE TABLE manager.infrastructure_templates (
    installation_id uuid NOT NULL REFERENCES manager.installations(id),
    id text COLLATE "C" NOT NULL CHECK(id ~ '^[a-z][a-z0-9-]{0,62}$'),
    definition jsonb NOT NULL CHECK(jsonb_typeof(definition)='object' AND octet_length(definition::text)<=16384),
    rejected_definition jsonb,
    condition text CHECK(condition IS NULL OR condition='invalid_definition'),
    PRIMARY KEY(installation_id,id)
);
CREATE TABLE manager.desired_users (
    installation_id uuid NOT NULL,
    subject text COLLATE "C" NOT NULL CHECK(octet_length(subject) BETWEEN 1 AND 255 AND subject ~ '^[!-~]+$'),
    template_id text COLLATE "C" NOT NULL,
    PRIMARY KEY(installation_id,subject),
    FOREIGN KEY(installation_id,template_id) REFERENCES manager.infrastructure_templates(installation_id,id)
);
ALTER TABLE manager.assignments
    ADD COLUMN infrastructure_managed boolean NOT NULL DEFAULT false,
    ADD COLUMN infrastructure_definition jsonb;
-- Source snapshots are frozen into versioned templates before a generation runs.
-- An infrastructure editor only needs DML on the two desired-state tables.
CREATE INDEX infrastructure_assignments ON manager.assignments(installation_id,id)
    WHERE infrastructure_managed;

-- A retired workload keeps its UID tombstone; a later explicit desired mapping
-- may reserve a new incarnation under the same name, never reuse the old UID.
ALTER TABLE manager.owned_resources DROP CONSTRAINT owned_resources_installation_id_kind_namespace_name_key;
CREATE UNIQUE INDEX owned_resources_active_name
    ON manager.owned_resources(installation_id,kind,namespace,name) WHERE state<>'Deleted';
