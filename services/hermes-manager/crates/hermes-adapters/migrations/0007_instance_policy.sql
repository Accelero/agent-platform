ALTER TABLE manager.owned_resources DROP CONSTRAINT owned_resources_kind_check;
ALTER TABLE manager.owned_resources ADD CONSTRAINT owned_resources_kind_check
    CHECK(kind IN ('Namespace','RoleBinding','ServiceAccount','PersistentVolumeClaim','Secret','ConfigMap','NetworkPolicy','Service','Deployment','Pod','InstancePolicy'));
