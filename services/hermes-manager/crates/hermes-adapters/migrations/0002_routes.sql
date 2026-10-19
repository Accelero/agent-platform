ALTER TABLE manager.instance_templates
    ADD COLUMN native_port integer NOT NULL DEFAULT 8080 CHECK (native_port BETWEEN 1 AND 65535),
    ADD COLUMN native_scheme text NOT NULL DEFAULT 'http' CHECK (native_scheme IN ('http', 'https'));

-- Only the controller will receive INSERT/UPDATE rights. Publishing this record
-- requires verified native owner binding, storage and network/admission policy.
-- API/router roles will only read it through the resolver.
CREATE TABLE manager.instance_routes (
    instance_id uuid PRIMARY KEY REFERENCES manager.instances(id),
    namespace_uid text NOT NULL CHECK (length(namespace_uid) BETWEEN 1 AND 255),
    service text NOT NULL CHECK (service = 'hermes'),
    port integer NOT NULL CHECK (port BETWEEN 1 AND 65535),
    scheme text NOT NULL CHECK (scheme IN ('http', 'https')),
    generation bigint NOT NULL CHECK (generation > 0),
    verified_owner boolean NOT NULL CHECK (verified_owner),
    verified_storage boolean NOT NULL CHECK (verified_storage),
    verified_policy boolean NOT NULL CHECK (verified_policy),
    verified_backend boolean NOT NULL CHECK (verified_backend),
    verified_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
