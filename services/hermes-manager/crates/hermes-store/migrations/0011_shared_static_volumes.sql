-- Static paths may be shared within and across assignments.
DROP TRIGGER volume_overlap ON manager.assignment_volumes;
DROP FUNCTION manager.guard_volume_overlap();
DROP INDEX manager.assignment_volumes_by_source;
