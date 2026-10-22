# Manager dashboard

The manager serves an authenticated React/TypeScript administration dashboard.
It provides searchable, filtered and paged assignments, users, terminal runtimes,
activity, storage bindings and PostgreSQL administrator grants; detail views;
typed command dialogs; template inspection; and synchronized form/JSON draft
authoring with immutable publication and explicit single-assignment or bulk apply.

Keycloak supplies verified identity. PostgreSQL grants are the sole authority
for administrator access. Infrastructure-owned templates and assignments remain
read-only in the dashboard. See the [feature specification](dashboard-features.md),
[stack conventions](dashboard-stack.md) and [work-package evidence](dashboard-workplan.md).

## Build and run

From the repository root:

```bash
uv sync --locked
uv run python -m agent_platform.manager_build
```

This builds `hermes-manager:local`: a pinned Node build stage installs the npm
lockfile, generates API types, tests and bundles the UI; a pinned Rust stage
builds both manager executables with the Cargo lockfile. The final Debian image
runs as UID/GID 10001 and contains the binaries, CA certificates, SSH client and
static UI. It contains no Node/Vite server. Only files explicitly enumerated in
`release-policy.json` under `development_contexts.hermes-manager` enter the build;
local environment files, deployment configuration and credentials are excluded.
Build diagnostics are bounded in `.local/hermes-manager-build.log`.

This is a local/CI image pipeline, separate from the existing five-image tagged
release. It does not publish an image or install the manager in the Helm chart.

Use [the API configuration example](../../services/hermes-manager/api.example.json)
as a nonsecret shape reference. Set `admin.uiDirectory` to
`/usr/share/hermes-manager/ui` in the image, or to the absolute `ui/dist` path
when running the Rust binary locally. Configure the existing installation,
PostgreSQL, TLS and browser-session inputs through private mounted files.
Initialize/migrate the registry through the existing manager commissioning
procedure before starting the API role. Then run:

```bash
hermes-manager api --config /run/manager/config.json
```

The dashboard is at `/` on the administration HTTPS listener, sharing its origin
with `/api/v1/...` and `/auth/...`. Resolver and execution listeners do not serve
it. Omit `admin.uiDirectory` for API-only mode. Enabling it without `admin.browser`,
or supplying a missing/invalid asset directory, fails startup.

## Keycloak identity and PostgreSQL administrator access

Use a dedicated Keycloak OIDC client, for example `manager-admin`, on a separate
administration hostname from personal Hermes. Configure an exact HTTPS callback
(`https://hermes-admin.example.org/auth/oidc/callback`), authorization-code flow
with S256 PKCE, public subject identifiers, the trusted issuer/JWKS endpoint and
an audience mapper matching `admin.audience`. Configure the authorized client IDs
and browser `clientId`; confidential clients use a private mounted secret file.

The manager verifies signature, issuer, subject, audience, authorized client,
user-session evidence, token type and validity. Token roles confer no manager
permission. Legacy `managementClient` and `administratorRole` configuration fields
are accepted for compatibility and ignored by authorization. No Keycloak
role-management credentials are needed.

Migration 0022 creates installation-scoped grants keyed by exact issuer/subject.
It creates no implicit administrators. After migrating, bootstrap the first
administrator before enabling access. Put a verified identity in a private file
outside the checkout and build context:

```json
{"issuer":"https://sso.example.org/realms/agents","subject":"<exact Keycloak subject>"}
```

Set `administratorIdentityFile` in the private API configuration, then run:

```bash
hermes-manager administrator-bootstrap --config /run/manager/config.json
```

Bootstrap succeeds only when no grant exists. For explicit operator recovery,
`hermes-manager administrator-recover --config /run/manager/config.json` restores
the configured exact identity's active management grant. Both commands audit the
action. They require database/operator access and never print the identity or
accept it as a command-line argument. Existing installations moving from role
checks need this bootstrap; there is no role fallback during migration.

An active grant permits administration; `canManageAdministrators` additionally
permits grant changes. The administrator screen displays PostgreSQL as the source
and selects exact identities observed through verified authentication using
searchable, bounded pages. An ungranted user can complete signed identity verification
without receiving an administrator session. Email and display names are never
identity keys. Removing or demoting the last active administrator capable of grant
management is refused. Grant edits use ETags, idempotency, audit and transactional
last-administrator protection.

Every page, asset and API request checks the current PostgreSQL grant, including
bearer requests. Mutation transactions recheck it under a lock. Revocation also
fences active browser sessions; it takes effect on the next request, independently
of token expiry or provider refresh. Already running requests can finish; revocation
cannot erase content already delivered to a browser. The UI rechecks its session
every 60 seconds and clears queries and unsaved forms on permission loss.

Anonymous navigation redirects to `/auth/login`. Provider tokens remain encrypted
server-side; browser cookies are opaque, Secure, HttpOnly and host-only. Mutations
require the current CSRF token and exact Origin as well as command preconditions.
Logout clears private in-memory state and opens `/auth/signed-out`. UI responses
use `no-store`, `nosniff`, `no-referrer` and a per-response style nonce. Script CSP
remains `script-src 'self'`; no inline script, CDN, dynamic JavaScript compilation
or blob worker is required. Unknown API/auth paths retain their error responses.

## Inspection and navigation

Users shows manager-registered identities and verified manager logins, with
provenance and their current assignment when present. It does not enumerate a
Keycloak directory. Open a user to create an assignment for that exact identity
or manage the existing one. Creation reviews the selected immutable template,
managed overrides and enforced platform settings before submission. Identity
links from a verified login continue to resolve after assignment creation.

Primary tables provide server search, supported filters, sorting and bounded
pages, with their query in the URL. Changing the query returns to the first page.
Paging retains a bounded window of recent cursors to keep shareable URLs small;
More can continue beyond that window, and First returns to the beginning. Failed
refreshes show a separate unavailable notice and hide stale rows.

Assignment details show desired and observed template versions, desired and
verified applied execution configurations, and paged operation history. Open an
operation for status and verified step timestamps. Activity provides redacted
change details and links to related records. Execution configuration events say
Recorded: this acknowledges their configuration write, not completed reconciliation.
Journal intent/result payloads and private user configuration are not exposed.

Open a terminal runtime for its complete immutable definition and version history,
then inspect assignments allowed to request that exact version or create a new
runtime version from it. Runtime and owner-specific binding selectors use
searchable manager references; binding suggestions stay scoped to the selected
instance's namespace and UID. Approved storage-class suggestions come from
published templates; Kubernetes availability is checked during reconciliation.

## Templates and rollout

Manager drafts live in dedicated authoring tables. Infrastructure definitions are
inspectable with safe redaction and diagnostics, but cannot be edited or adopted.
Published templates and saved drafts have searchable paged collections. Inspect
immutable versions, compare their definitions, clone one into a new template or
start the next revision of a manager-owned template. Cloning copies the generic
managed definition into a draft and preserves advanced fields.

A published draft becomes an immutable catalog version; publication never updates
existing assignments. Review the semantic configuration diff, select an exact
version and explicitly apply it. Access, issuer/subject, instance identity, retained
PVC data and native credentials remain scoped to the same owner.

Apply previews show resources, managed overrides, storage and restart effects.
Storage-class changes, shrinking and infrastructure source conflicts block dispatch.
Enabled instances drain execution and reconcile through the native Recreate path.
Bulk apply includes only selected IDs and their reviewed versions (maximum 50).
Its durable result page distinguishes dispatch acceptance from each instance's
operation. Pending dispatch can be resumed by the original administrator without
reapplying accepted members. Polling backs off and stops after ten minutes or a
terminal result; reopening the durable page starts a fresh observation window.

The JSON editor uses CodeMirror 6 and preserves unknown Hermes fields, explicit
`null` and omitted values when switching forms. Invalid JSON blocks the form switch.
Schemas are local and versioned; Ajv validators are compiled at build time. The
manager envelope is validated completely; the Hermes schema covers reviewed keys
for its recorded image digest. Other Hermes keys and unknown images are explicitly
reported as unvalidated. Server validation remains authoritative, and the UI
reports schema digest incompatibility. Drafts are kept in memory until saved;
there is no default browser-storage persistence. Personal prompts, credentials,
profiles and memory must not be put in shared template configuration.

## Frontend development and checks

Use Node 26.11.1 and npm 11.20.0, pinned by the image and frontend configuration:

```bash
cd services/hermes-manager/ui
npm ci --ignore-scripts
npm run check:generated
npm run check:schemas
npm run format:check
npm test
npm run build
npx --no-install playwright install --with-deps chromium
npm run test:e2e
```

API types are generated from the repository's administration OpenAPI contract.
CI rejects stale generated types and checks the TypeScript/build, browser
workflows, Rust OIDC/PostgreSQL-grant/asset authorization, compiled process and full
multi-stage image pipeline. The browser presentation tests use synthetic API
fixtures; Rust protocol tests use real HTTPS and PostgreSQL with a signed OIDC
provider fixture. The separate production browser test uses Rust's built asset server, real HTTPS,
PostgreSQL and the signed provider fixture, with cookie/CSRF writes and CSP checks:

```bash
cd services/hermes-manager
MANAGER_UI_DIST="$PWD/ui/dist" DATABASE_URL=postgres://hermes_test@127.0.0.1:5432/hermes_test \
  cargo test -p hermes-adapters --test browser production_dashboard_browser_csp --locked -- --ignored
```

These fixtures do not provision a deployed Keycloak realm.

`npm run dev` starts the local Vite development server. For authenticated HMR,
serve it over HTTPS using `MANAGER_DEV_TLS_CERT_FILE` and
`MANAGER_DEV_TLS_KEY_FILE` from outside the checkout/build context. Set
`MANAGER_DEV_ORIGIN` to the Rust administration HTTPS listener and use
`NODE_EXTRA_CA_CERTS` if its CA is private; certificate verification stays enabled.
Set the manager browser `publicOrigin` to that development HTTPS origin (for
example `https://hermes-admin.localhost:5173`), add its exact callback in the
development Keycloak client and use the same origin in the browser. Vite proxies
`/api` and `/auth` to Rust; Rust still performs every authorization check. The
plain HTTP development server used by presentation tests is not the production
authentication path. No client secret belongs in a `VITE_` variable or UI file.
