-- Keyset pagination's default order uses installation-scoped indexes.
CREATE INDEX dashboard_assignment_order ON manager.assignments(installation_id,id);
CREATE INDEX dashboard_audit_order ON manager.audit_events(installation_id,id);
