-- Activation remains fail-closed: migration 0008 still forces applied_generation=0.
-- A later trusted activation implementation may write these immutable receipts.
CREATE TABLE manager.execution_effective_policy_receipts (
 instance_id uuid NOT NULL, generation bigint NOT NULL, epoch bigint NOT NULL CHECK(epoch>0),
 content_digest text NOT NULL CHECK(content_digest ~ '^[0-9a-f]{64}$'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(instance_id,generation,epoch),
 FOREIGN KEY(instance_id,generation) REFERENCES manager.execution_policy_versions(instance_id,generation)
);
CREATE TRIGGER immutable_execution_effective_receipt BEFORE UPDATE OR DELETE ON manager.execution_effective_policy_receipts FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.execution_environments (
 id uuid PRIMARY KEY, installation_id uuid NOT NULL REFERENCES manager.installations(id),
 instance_id uuid NOT NULL REFERENCES manager.instances(id), identity_digest bytea NOT NULL CHECK(octet_length(identity_digest)=32),
 generation bigint NOT NULL DEFAULT 1 CHECK(generation>0), epoch bigint NOT NULL CHECK(epoch>0),
 assignment_generation bigint NOT NULL CHECK(assignment_generation>0), policy_generation bigint NOT NULL,
 policy_digest text NOT NULL CHECK(policy_digest ~ '^[0-9a-f]{64}$'),
 runtime_id text COLLATE "C" NOT NULL, profile text COLLATE "C" NOT NULL CHECK(profile='default'),
 session text COLLATE "C" NOT NULL, mode text NOT NULL CHECK(mode IN ('per_session','per_profile_on_demand','per_profile_always_running')),
 limits jsonb NOT NULL, invocation jsonb, state text NOT NULL DEFAULT 'Pending' CHECK(state IN ('Pending','Provisioning','Ready','Draining','Retired')),
 pod_name text COLLATE "C" NOT NULL, pod_uid text, resource_version text, spec_digest text, ssh_host text,
 fence bigint NOT NULL DEFAULT 0 CHECK(fence>=0), lease_owner uuid, lease_until timestamptz,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), last_used_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 drain_deadline timestamptz,
 UNIQUE(instance_id,identity_digest), UNIQUE(id,generation), UNIQUE(instance_id,id),
 FOREIGN KEY(instance_id,policy_generation,epoch) REFERENCES manager.execution_effective_policy_receipts(instance_id,generation,epoch),
 CHECK((lease_owner IS NULL)=(lease_until IS NULL)),
 CHECK((pod_uid IS NULL)=(ssh_host IS NULL)),
 CHECK(state<>'Ready' OR (pod_uid IS NOT NULL AND spec_digest IS NOT NULL AND resource_version IS NOT NULL)),
 CHECK(state NOT IN ('Draining','Retired') OR drain_deadline IS NOT NULL)
);
CREATE TABLE manager.execution_worker_intents (
 environment_id uuid NOT NULL,generation bigint NOT NULL,epoch bigint NOT NULL,fence bigint NOT NULL,
 spec_digest text NOT NULL CHECK(spec_digest ~ '^[0-9a-f]{64}$'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(environment_id,generation), FOREIGN KEY(environment_id,generation) REFERENCES manager.execution_environments(id,generation)
);
CREATE TRIGGER immutable_execution_worker_intent BEFORE UPDATE OR DELETE ON manager.execution_worker_intents FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.execution_worker_ready_receipts (
 environment_id uuid NOT NULL,generation bigint NOT NULL,pod_uid text NOT NULL,resource_version text NOT NULL,
 spec_digest text NOT NULL CHECK(spec_digest ~ '^[0-9a-f]{64}$'),observed_spec_digest text NOT NULL CHECK(observed_spec_digest ~ '^[0-9a-f]{64}$'),ssh_host text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),PRIMARY KEY(environment_id,generation),
 FOREIGN KEY(environment_id,generation) REFERENCES manager.execution_worker_intents(environment_id,generation)
);
CREATE TRIGGER immutable_execution_worker_ready BEFORE UPDATE OR DELETE ON manager.execution_worker_ready_receipts FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.execution_requests (
 client_id uuid NOT NULL REFERENCES manager.execution_clients(id), request text COLLATE "C" NOT NULL CHECK(length(request) BETWEEN 1 AND 256),
 body_digest bytea NOT NULL CHECK(octet_length(body_digest)=32),environment_id uuid NOT NULL REFERENCES manager.execution_environments(id),
 environment_generation bigint NOT NULL, lease_id uuid NOT NULL UNIQUE, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(client_id,request), FOREIGN KEY(environment_id,environment_generation) REFERENCES manager.execution_environments(id,generation)
);
CREATE TRIGGER immutable_execution_request BEFORE UPDATE OR DELETE ON manager.execution_requests FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
CREATE TABLE manager.execution_leases (
 id uuid PRIMARY KEY,client_id uuid NOT NULL REFERENCES manager.execution_clients(id),instance_id uuid NOT NULL,
 environment_id uuid NOT NULL,environment_generation bigint NOT NULL,epoch bigint NOT NULL CHECK(epoch>0),
 parent_lease uuid REFERENCES manager.execution_leases(id),exclusive boolean NOT NULL DEFAULT false,
 state text NOT NULL DEFAULT 'Active' CHECK(state IN ('Active','Released','Expired')),
 granted_at timestamptz NOT NULL DEFAULT clock_timestamp(),original_expires_at timestamptz NOT NULL,expires_at timestamptz NOT NULL,
 fence bigint NOT NULL DEFAULT 1 CHECK(fence>0),terminal_at timestamptz,
 FOREIGN KEY(instance_id,environment_id) REFERENCES manager.execution_environments(instance_id,id),
 FOREIGN KEY(environment_id,environment_generation) REFERENCES manager.execution_environments(id,generation),
 CHECK(expires_at>=original_expires_at),CHECK((state='Active')=(terminal_at IS NULL))
);
CREATE INDEX execution_leases_due ON manager.execution_leases(expires_at) WHERE state='Active';
CREATE INDEX execution_leases_environment ON manager.execution_leases(environment_id,state,expires_at);
CREATE FUNCTION manager.guard_execution_lease() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'lease tombstone required' USING ERRCODE='23514'; END IF;
 IF (to_jsonb(NEW)-'expires_at'-'state'-'terminal_at'-'fence') IS DISTINCT FROM (to_jsonb(OLD)-'expires_at'-'state'-'terminal_at'-'fence')
 OR OLD.state<>'Active' OR NEW.expires_at<OLD.expires_at OR NEW.fence<>OLD.fence+1 THEN
 RAISE EXCEPTION 'immutable lease provenance or terminal state' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_execution_lease BEFORE UPDATE OR DELETE ON manager.execution_leases FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_lease();
CREATE FUNCTION manager.guard_execution_environment() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'environment history required' USING ERRCODE='23514'; END IF;
 IF (to_jsonb(NEW)-'state'-'pod_uid'-'resource_version'-'spec_digest'-'ssh_host'-'fence'-'lease_owner'-'lease_until'-'last_used_at'-'drain_deadline') IS DISTINCT FROM (to_jsonb(OLD)-'state'-'pod_uid'-'resource_version'-'spec_digest'-'ssh_host'-'fence'-'lease_owner'-'lease_until'-'last_used_at'-'drain_deadline')
 OR NEW.fence<OLD.fence OR NEW.fence>OLD.fence+1 OR (OLD.state IN ('Draining','Retired') AND NEW.state NOT IN ('Draining','Retired'))
 OR (OLD.state='Ready' AND NEW.state NOT IN ('Ready','Draining'))
 OR (NEW.lease_owner IS NOT NULL AND (NEW.lease_owner IS DISTINCT FROM OLD.lease_owner OR OLD.state<>'Provisioning') AND NEW.fence<>OLD.fence+1)
 OR (OLD.drain_deadline IS NOT NULL AND NEW.drain_deadline IS DISTINCT FROM OLD.drain_deadline)
 OR (OLD.pod_uid IS NOT NULL AND (NEW.pod_uid IS DISTINCT FROM OLD.pod_uid OR NEW.spec_digest IS DISTINCT FROM OLD.spec_digest OR NEW.ssh_host IS DISTINCT FROM OLD.ssh_host)) THEN
 RAISE EXCEPTION 'immutable environment provenance' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_execution_environment BEFORE UPDATE OR DELETE ON manager.execution_environments FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_environment();

CREATE TABLE manager.execution_worker_start_intents (
 environment_id uuid NOT NULL,generation bigint NOT NULL,pod_uid text NOT NULL,resource_version text NOT NULL,
 spec_digest text NOT NULL CHECK(spec_digest ~ '^[0-9a-f]{64}$'),observed_spec_digest text NOT NULL CHECK(observed_spec_digest ~ '^[0-9a-f]{64}$'),created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(environment_id,generation),FOREIGN KEY(environment_id,generation) REFERENCES manager.execution_worker_intents(environment_id,generation)
);
CREATE TRIGGER immutable_execution_worker_start BEFORE UPDATE OR DELETE ON manager.execution_worker_start_intents FOR EACH ROW EXECUTE FUNCTION manager.reject_change();
-- Keep first UID binding immutable. Every refreshed CAS resource version is a
-- separate committed action input, never an unrecorded write precondition.
CREATE TABLE manager.execution_worker_start_inputs (
 environment_id uuid NOT NULL,generation bigint NOT NULL,resource_version text NOT NULL,
 pod_uid text NOT NULL,spec_digest text NOT NULL CHECK(spec_digest ~ '^[0-9a-f]{64}$'),
 observed_spec_digest text NOT NULL CHECK(observed_spec_digest ~ '^[0-9a-f]{64}$'),
 epoch bigint NOT NULL CHECK(epoch>0),fence bigint NOT NULL CHECK(fence>0),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(environment_id,generation,resource_version),
 FOREIGN KEY(environment_id,generation) REFERENCES manager.execution_worker_start_intents(environment_id,generation)
);
CREATE TRIGGER immutable_execution_worker_start_input BEFORE UPDATE OR DELETE ON manager.execution_worker_start_inputs FOR EACH ROW EXECUTE FUNCTION manager.reject_change();

CREATE FUNCTION manager.guard_execution_lease_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 PERFORM 1 FROM manager.execution_environments WHERE id=NEW.environment_id FOR UPDATE;
 IF NOT EXISTS(SELECT 1 FROM manager.execution_clients c JOIN manager.execution_environments e ON e.instance_id=c.instance_id WHERE c.id=NEW.client_id AND c.instance_id=NEW.instance_id AND e.id=NEW.environment_id AND e.generation=NEW.environment_generation AND e.epoch=NEW.epoch)
 OR EXISTS(SELECT 1 FROM manager.execution_leases l WHERE l.environment_id=NEW.environment_id AND l.state='Active' AND l.expires_at>clock_timestamp() AND (l.exclusive OR NEW.exclusive)) THEN
 RAISE EXCEPTION 'lease scope or exclusive conflict' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER execution_lease_insert BEFORE INSERT ON manager.execution_leases FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_lease_insert();
CREATE FUNCTION manager.guard_execution_effective_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF NOT EXISTS(SELECT 1 FROM manager.execution_policy_versions v JOIN manager.instances i ON i.id=v.instance_id JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.installations p ON p.id=a.installation_id WHERE v.instance_id=NEW.instance_id AND v.generation=NEW.generation AND v.content_digest=NEW.content_digest AND p.epoch=NEW.epoch) THEN
 RAISE EXCEPTION 'effective receipt scope or digest mismatch' USING ERRCODE='23514'; END IF; RETURN NEW;
END $$;
CREATE TRIGGER execution_effective_receipt_insert BEFORE INSERT ON manager.execution_effective_policy_receipts FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_effective_receipt();
CREATE FUNCTION manager.guard_execution_ready_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF NOT EXISTS(SELECT 1 FROM manager.execution_worker_start_intents b JOIN manager.execution_worker_intents w USING(environment_id,generation) WHERE b.environment_id=NEW.environment_id AND b.generation=NEW.generation AND b.pod_uid=NEW.pod_uid AND b.spec_digest=NEW.spec_digest AND b.observed_spec_digest=NEW.observed_spec_digest AND w.spec_digest=NEW.spec_digest) THEN
 RAISE EXCEPTION 'ready receipt requires exact bound start intent' USING ERRCODE='23514'; END IF; RETURN NEW;
END $$;
CREATE TRIGGER execution_ready_receipt_insert BEFORE INSERT ON manager.execution_worker_ready_receipts FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_ready_receipt();
-- Re-use the newest committed Namespace observation, never a historical Open
-- beneath a later Closed result. The trusted adapter also validates its digest.
CREATE FUNCTION manager.execution_namespace_open(target uuid,target_epoch bigint,target_generation bigint,target_uid text)
RETURNS boolean LANGUAGE sql STABLE AS $$
 SELECT COALESCE((SELECT n.input->'state'->>'admission'='open'
 AND n.input->'state'->>'access'='enabled'
 AND n.input->'state'->>'epoch'=target_epoch::text
 AND n.input->'state'->>'generation'=target_generation::text
 AND n.input->>'namespaceUid'=target_uid AND s.result->>'status'='verified' AND s.result->>'uid'=target_uid
 FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s USING(operation_id,step_key)
 JOIN manager.owned_resources r ON r.id=n.resource_id WHERE r.instance_id=target AND s.verified_at IS NOT NULL
 ORDER BY s.verified_at DESC,s.recorded_at DESC,n.step_key DESC LIMIT 1),false)
$$;
-- Parent references are identities, never authority to cross client/environment.
CREATE FUNCTION manager.guard_execution_parent() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF NEW.parent_lease IS NOT NULL AND NOT EXISTS(SELECT 1 FROM manager.execution_leases p WHERE p.id=NEW.parent_lease AND p.client_id=NEW.client_id AND p.instance_id=NEW.instance_id AND p.environment_id=NEW.environment_id AND p.environment_generation=NEW.environment_generation AND p.epoch=NEW.epoch AND p.state='Active' AND p.expires_at>clock_timestamp()) THEN
 RAISE EXCEPTION 'parent lease is not live in this scope' USING ERRCODE='23514'; END IF; RETURN NEW;
END $$;
CREATE TRIGGER execution_parent_insert BEFORE INSERT ON manager.execution_leases FOR EACH ROW EXECUTE FUNCTION manager.guard_execution_parent();
