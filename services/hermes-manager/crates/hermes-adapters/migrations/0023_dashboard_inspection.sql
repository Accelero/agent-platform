CREATE INDEX dashboard_operation_history ON manager.operations(installation_id,instance_id,id);
CREATE INDEX dashboard_draft_order ON manager.dashboard_template_drafts(installation_id,id);
CREATE INDEX dashboard_audit_assignment_order ON manager.audit_events(installation_id,assignment_id,id);

-- Execution configuration events are acknowledgements, not lifecycle operations.
CREATE VIEW manager.dashboard_activity AS
SELECT id,installation_id,actor_issuer,actor_subject,action,assignment_id,operation_id,
       entity_type,entity_id,entity_operation_id,request_id,version,change,occurred_at,false AS execution_event
FROM manager.audit_events
UNION ALL
SELECT e.id,e.installation_id,e.actor_issuer,e.actor_subject,e.action,
       coalesce((e.change->>'assignmentId')::uuid,i.assignment_id),NULL::uuid,
       CASE WHEN e.change ? 'runtimeId' OR e.change ? 'runtime' THEN 'runtime'
            WHEN e.change ? 'storageBindingId' THEN 'storage_binding'
            WHEN e.change ? 'assignmentId' THEN 'assignment' ELSE 'instance' END,
       coalesce(e.change->>'runtimeId',e.change->'runtime'->>'id',e.change->>'storageBindingId',e.change->>'assignmentId',e.change->>'instanceId'),
       NULL::uuid,e.request_id,coalesce((e.result->>'assignmentVersion')::bigint,1),
       CASE WHEN e.change ? 'runtime' THEN e.change || jsonb_build_object('runtimeId',e.change->'runtime'->>'id','version',e.change->'runtime'->'version')
            WHEN e.change ? 'name' THEN e.change || jsonb_build_object('pullSecretName',e.change->>'name') ELSE e.change END,
       e.occurred_at,true
FROM manager.execution_configuration_events e
LEFT JOIN manager.instances i ON i.id=(e.change->>'instanceId')::uuid;

CREATE INDEX dashboard_catalog_version_order ON manager.instance_templates((id||':'||lpad(version::text,20,'0'))) WHERE approved AND schema_version=1;

CREATE INDEX dashboard_verified_identity_order ON manager_admin_auth.verified_identities(installation_id,id);
