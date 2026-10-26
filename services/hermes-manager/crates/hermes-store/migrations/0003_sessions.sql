-- Browser sessions. `hermes-identity` owns the queries; the tables live in the
-- single migration history. Each schema can be granted to its own database
-- role: the gateway's role needs only `gateway_sessions`.
--
-- Provider tokens and login secrets are sealed with AEAD before they reach
-- these tables; the associated data binds the installation, purpose, record
-- kind, record hash and key ID. Rows hold only hashes of cookie values.

CREATE SCHEMA admin_sessions;
CREATE SCHEMA gateway_sessions;
REVOKE ALL ON SCHEMA admin_sessions, gateway_sessions FROM PUBLIC;

-- One row per browser proof cookie. Login and logout advance `generation`,
-- which invalidates every older flow and session of that browser.
CREATE TABLE admin_sessions.browsers (
    id_hash bytea PRIMARY KEY CHECK (octet_length(id_hash) = 32),
    generation bigint NOT NULL CHECK (generation > 0),
    expires_at timestamptz NOT NULL
);

-- Single-use authorization-code flows.
CREATE TABLE admin_sessions.flows (
    state_hash bytea PRIMARY KEY CHECK (octet_length(state_hash) = 32),
    browser_hash bytea NOT NULL REFERENCES admin_sessions.browsers (id_hash),
    browser_generation bigint NOT NULL CHECK (browser_generation > 0),
    key_id text NOT NULL CHECK (length(key_id) BETWEEN 1 AND 63),
    nonce bytea NOT NULL CHECK (octet_length(nonce) = 24),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 16 AND 65536),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    session_hash bytea CHECK (session_hash IS NULL OR octet_length(session_hash) = 32)
);

-- `refreshing` is a fenced claim: one process refreshes provider tokens while
-- others wait or fail closed; an interrupted refresh revokes the session.
CREATE TABLE admin_sessions.sessions (
    id_hash bytea PRIMARY KEY CHECK (octet_length(id_hash) = 32),
    browser_hash bytea NOT NULL REFERENCES admin_sessions.browsers (id_hash),
    browser_generation bigint NOT NULL CHECK (browser_generation > 0),
    issuer text COLLATE "C" NOT NULL,
    subject text COLLATE "C" NOT NULL CHECK (octet_length(subject) BETWEEN 1 AND 255),
    key_id text NOT NULL CHECK (length(key_id) BETWEEN 1 AND 63),
    nonce bytea NOT NULL CHECK (octet_length(nonce) = 24),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 16 AND 65536),
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'refreshing', 'revoked')),
    verified_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    refresh_owner uuid,
    refresh_until timestamptz,
    fence bigint NOT NULL DEFAULT 0 CHECK (fence >= 0),
    CHECK ((state = 'refreshing') = (refresh_owner IS NOT NULL)),
    CHECK ((refresh_owner IS NULL) = (refresh_until IS NULL))
);

CREATE INDEX admin_flows_browser ON admin_sessions.flows (browser_hash);
CREATE INDEX admin_flows_expiry ON admin_sessions.flows (expires_at);
CREATE INDEX admin_sessions_browser ON admin_sessions.sessions (browser_hash);
CREATE INDEX admin_sessions_expiry ON admin_sessions.sessions (expires_at);
CREATE INDEX admin_browsers_expiry ON admin_sessions.browsers (expires_at);

CREATE TABLE gateway_sessions.browsers (LIKE admin_sessions.browsers INCLUDING ALL);
CREATE TABLE gateway_sessions.flows (LIKE admin_sessions.flows INCLUDING ALL);
ALTER TABLE gateway_sessions.flows
    ADD FOREIGN KEY (browser_hash) REFERENCES gateway_sessions.browsers (id_hash);
CREATE TABLE gateway_sessions.sessions (LIKE admin_sessions.sessions INCLUDING ALL);
ALTER TABLE gateway_sessions.sessions
    ADD FOREIGN KEY (browser_hash) REFERENCES gateway_sessions.browsers (id_hash);
