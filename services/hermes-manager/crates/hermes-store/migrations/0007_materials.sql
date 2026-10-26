-- Instance material: credentials and keys generated once per instance and
-- purpose, sealed to the controller's key, inserted before any use. Rows are
-- never replaced or deleted (domain rule: material is created once).
CREATE TABLE manager.materials (
    instance_id uuid NOT NULL REFERENCES manager.instances (id),
    purpose text NOT NULL CHECK (purpose IN
        ('dashboard-session', 'execution-credential', 'ssh-client', 'worker-host', 'initialization')),
    id uuid NOT NULL UNIQUE,
    sealed bytea NOT NULL CHECK (octet_length(sealed) BETWEEN 1 AND 16384),
    public text CHECK (public IS NULL OR length(public) BETWEEN 1 AND 4096),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (instance_id, purpose)
);
CREATE TRIGGER materials_immutable BEFORE UPDATE OR DELETE ON manager.materials
    FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

-- Recorded status never claims more than is desired (domain rule: status is
-- the controller's observation of a desired generation).
ALTER TABLE manager.instances
    ADD CONSTRAINT instance_observed_not_ahead CHECK (observed_generation <= desired_generation);
