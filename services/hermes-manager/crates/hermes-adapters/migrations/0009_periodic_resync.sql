-- Due observations survive missed notifications and process death. Completion
-- never reopens the original command or changes desired/observed generation.
ALTER TABLE manager.instances ADD COLUMN reconcile_after timestamptz NOT NULL
    DEFAULT clock_timestamp();
CREATE INDEX instances_reconcile_due ON manager.instances(reconcile_after,id)
    WHERE state IN ('Ready','Suspended','Retained');

ALTER TABLE manager.operations DROP CONSTRAINT operations_instance_id_generation_key;
CREATE UNIQUE INDEX operations_command_generation ON manager.operations(instance_id,generation)
    WHERE kind<>'resync';
-- Every kind shares the same fenced work queue. A new command supersedes
-- preceding work before inserting its operation; resync cannot race it.
CREATE UNIQUE INDEX operations_active_instance ON manager.operations(instance_id)
    WHERE state IN ('Pending','Running','Waiting','RetryAt');
ALTER TABLE manager.operations DROP CONSTRAINT operations_kind_check;
ALTER TABLE manager.operations ADD CONSTRAINT operations_kind_check
    CHECK(kind IN ('commission','suspend','revoke','retry','resync'));
