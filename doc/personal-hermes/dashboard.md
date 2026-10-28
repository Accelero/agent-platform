# Manager dashboard

The manager serves an authenticated React/TypeScript administration dashboard
from its Rust API role. Administrators use it to manage:

- who gets a Hermes instance (assignments);
- Hermes templates;
- terminal backends;
- volumes;
- admin secrets;
- administrator grants.

Keycloak supplies verified identity. PostgreSQL grants are the only authority
for administrator access. The dashboard uses only the administration API, and
every workflow is also available to an API-only client.

## Views

Every collection uses server-side search, filters, sorting and bounded pages,
with the query in the URL. Changing the query returns to the first page.
Loading, empty, denied, unavailable and stale states are shown separately. A
failed query never looks like an empty database.

| View | Shows | Actions |
| --- | --- | --- |
| Overview | What the platform does and where to start | Open the other views |
| Users | Known identities (exact issuer/subject, labels, assignment and instance state) and, when configured, a live Keycloak directory tab | Create an assignment for an exact identity, or open the existing one |
| Assignments | Identity, access, template version, volumes, allowed terminal backends and default, lifetime mode, admin overrides, secret references; the instance's phase, conditions and generations; its terminal environments with state, lease count and a failed worker's reason | Change access, template version, volumes, backends, overrides or secret references after a preview; retry a blocked instance; set a template version for selected assignments |
| Hermes templates | Template IDs with version count, latest published version, current draft and assignment count; each version's state, revision and definition | Start a draft, or the next version from an existing one; edit drafts as a form, JSON or YAML; validate; publish; withdraw |
| Terminal backends | The same catalog view for terminal backend versions: image, SSH contract, UID/GID, resources, limits, mount slots and registry secret | The same as templates |
| Storage | Installation storage sources (read-only, from Helm values), the volumes on each by instance and path, and orphaned namespaces and volumes with first- and last-seen times | Open the owning assignment |
| Secrets | Admin secret names, kinds, latest version, update time and reference count — never values | Create, write a new version, delete when unreferenced |
| Administrators | Exact identities with grants and their version | Grant, change or revoke (requires the manage-administrators grant) |
| Activity | Audit events: actor, action, target, request ID and time | Filter by target; open an event's redacted change details |

Exact `(issuer, subject)` is authoritative everywhere. Email and display names
are labels and search aids, never ownership or authorization keys. A user does
not need an assignment to be an administrator, and an assignment never grants
administration.

## Build and run

From the repository root:

```bash
uv sync --locked
uv run python -m agent_platform.manager_build
```

This builds `hermes-manager:local`. A pinned Node stage installs the npm
lockfile, generates API types, tests and bundles the UI; a pinned Rust stage
builds `hermes-manager` and `hermes-gateway` with the Cargo lockfile. The final
Debian image runs as UID/GID 10001 with the binaries, CA certificates and the
static UI, and no Node/Vite server. Only files listed under
`development_contexts.hermes-manager` in `release-policy.json` enter the build.
Build diagnostics are bounded in `.local/hermes-manager-build.log`. The same
input set is the `hermes-manager` release target, and the chart installs it; see
the [chart guide](../../charts/agent-platform/README.md).

The chart generates the `api` role's configuration. Its `administration`
section sets `dashboardDirectory` to `/usr/share/hermes-manager/ui` when browser
sessions are enabled (`hermesManager.api.browser.enabled`), the optional
`directory`, and `secretSealingPublicKeyFile`, the controller's 32-byte X25519
public key that admin secrets are sealed to. The installation's
`storageSources` and `dataIdentity` come from Helm values, and the dashboard
shows them read-only. When running the binary locally, point
`dashboardDirectory` at the absolute `ui/dist` path:

```bash
hermes-manager api --config /run/hermes/config/config.json
```

The dashboard is at `/` on the administration HTTPS listener, sharing its origin
with `/api/v1/...` and `/auth/...`. Resolver and execution listeners do not serve
it. Without browser sessions the API runs API-only; a dashboard directory
without browser sessions, or a missing or invalid asset directory, fails
startup.

## Keycloak identity and PostgreSQL administrator access

Use a dedicated Keycloak OIDC client, for example `manager-admin`, on a separate
administration hostname from personal Hermes. Configure an exact HTTPS callback
(`https://hermes-admin.example.org/auth/oidc/callback`), authorization-code flow
with S256 PKCE, public subject identifiers, the trusted issuer/JWKS endpoint and
an audience mapper matching `hermesManager.api.tokens.audience`. Configure the
authorized client IDs and the browser `clientId`; a confidential client's secret
is the `oidc-client-secret` key of the API's Secret.

The manager verifies signature, issuer, subject, audience, authorized client,
user-session evidence, token type and validity. Token roles grant no manager
permission, and no Keycloak role-management credentials are needed.

Grants are installation-scoped and keyed by exact issuer/subject. A fresh
installation has no administrators. Grant the first one with the
`operations.grantAdministrator` Job, whose Secret holds the identity file, kept
outside the checkout and build context:

```json
{"issuer":"https://sso.example.org/realms/agents","subject":"<exact Keycloak subject>"}
```

The Job runs:

```bash
hermes-manager grant-administrator --config /run/hermes/config/config.json \
  --identity-file /run/hermes/secrets/identity.json
```

By default the command succeeds only while no grant exists. `--recover`
restores an active manage-administrators grant for that exact identity when
grants already exist. Both forms are audited, require database and operator
access, and never print the identity or accept it as a command-line argument.

An active grant permits administration; the manage-administrators flag
additionally permits grant changes. Grant targets must have verified login
evidence. Grant edits use ETags, actor-scoped idempotency, audit and
transactional protection against removing or demoting the last administrator
who can manage grants.

Every page, asset and API request checks the current PostgreSQL grant, including
bearer requests, and mutation transactions recheck it under a lock. Revocation
fences active browser sessions and takes effect on the next request, independently
of token expiry. Already running requests can finish. The UI rechecks its session
every 60 seconds and clears queries and unsaved forms on permission loss.

Anonymous navigation redirects to `/auth/login`. Provider tokens stay encrypted
server-side; browser cookies are opaque, Secure, HttpOnly and host-only. Mutations
require the current CSRF token and exact Origin as well as version and
idempotency preconditions. Logout clears in-memory state and opens
`/auth/signed-out`. UI responses use `no-store`, `nosniff`, `no-referrer` and a
per-response style nonce; script CSP stays `script-src 'self'` with no inline
script, CDN, dynamic code compilation or blob worker.

## Inspection

- **Users.** Open a user to create an assignment for that exact identity, or
  to manage the existing one. Creation reviews the selected template version,
  the managed settings layers, the volumes and the terminal backends before
  submission.
- **Assignments.** Assignment details show:
  - desired and observed generation;
  - the instance phase and its conditions (storage, native initialization,
    native authentication, terminal);
  - each volume with its source, path or size and where it is mounted;
  - its terminal environments with their state, lease count and, for a
    failed worker, the reason (no addresses or keys).

  Activity shows redacted change details. A recorded command means the change
  was accepted, not that reconciliation has completed.
- **Never shown.** Private user configuration, secret values and command
  content.
- **Validation before saving.** When editing an assignment, the dashboard
  validates source paths for overlap with other instances, required mount
  slots and mount paths shared between Hermes and backends. It shows the
  server's errors (`volume_overlap`, `mount_slot_unbound`,
  `mount_path_mismatch`) on the affected fields.

## Templates, publication and rollout

A Hermes template version holds the instance definition and its admin settings.
See [instances](instances.md#hermes-templates) for the fields:

```json
{
  "schemaVersion": 2,
  "image": "registry.example.org/platform/hermes@sha256:<64 lowercase hex digits>",
  "resources": { "cpuMillis": 1000, "memoryMib": 2048 },
  "home": { "source": "nvme", "sizeGib": 5 },
  "mounts": { "workspace": { "mountPath": "/workspace" } },
  "defaultVolumes": { "workspace": { "source": "nvme", "sizeGib": 10 } },
  "adminConfig": { "agent": { "max_turns": 30 } },
  "secrets": [{ "name": "llm-gateway-key", "env": "OPENAI_API_KEY" }]
}
```

The managed configuration preview distinguishes four layers:

1. **Platform-enforced settings:**
   - `terminal.backend=managed_execution`;
   - the image-owned plugin;
   - protected connection paths.

   No editor can change them, and no credential value is shown.
2. **Assignment overrides.**
3. **Template `adminConfig`**, such as a turn limit, a pinned LLM provider or a
   pinned default `terminal.runtime_id`.
4. **Settings left to the user.** Removing an override can expose the user's
   retained value again; it never erases their configuration.

Choosing a default backend in a template grants no backend permission; the
assignment's allowed backends decide. `adminConfig` must not contain model
keys, passwords, personal prompts or other private seed data. Use admin
secrets for values.

Template versions:

- **Draft versions** are edited in place.
- **Publishing** validates a draft and makes it immutable. It never changes
  existing assignments.
- **Withdrawing** a version prevents new selections.

To roll out, select the exact version and apply it explicitly. The apply
preview shows resources, settings, storage and restart effects. Storage-class
changes and shrinking block the change.

Bulk set-template:

- applies one reviewed version to explicitly selected assignments (at most
  50);
- the response reports each assignment's result: accepted with its new
  version, or rejected with a reason;
- accepted changes are ordinary assignment changes, whose progress shows in
  each instance's status;
- rejected members can be retried with a new request.

Access, identity, instance, volumes and native credentials stay with the same
owner throughout.

Terminal backend versions follow the same Draft, Published and Withdrawn
workflow. See [terminal backends](terminal-backends.md#define-a-terminal-backend-administrator)
for the definition.

## Form, JSON and YAML authoring

Form, JSON and YAML edit one draft and send canonical JSON to the validation,
save and publish APIs. Switching modes preserves unknown Hermes fields, explicit
`null` and omitted values; invalid raw input is retained and blocks mode changes
and writes until corrected. Drafts stay in memory until saved, with unsaved-change
navigation protection.

YAML accepts only JSON-compatible input: a single mapping document with unique
string keys, finite safe numbers and bounded size and depth. Anchors, aliases,
merge keys, explicit tags and multi-document streams are rejected. Comments and
formatting may be discarded on conversion or save. YAML never becomes an
arbitrary Kubernetes manifest or a separate server-side format.

Schemas are local and versioned, with Ajv validators precompiled at build time.
The manager envelope is validated completely; the Hermes schema covers reviewed
keys for its recorded image digest, and other keys or unknown images are
reported as unvalidated. Rust validation is authoritative. Errors carry a JSON
Pointer, code, severity and safe message, mapped to form fields and editor
positions.

## Read-only Keycloak directory

Optionally enable `hermesManager.api.directory` with its client ID; the client
secret is the `directory-client-secret` key of the API's Secret. The generated
configuration then contains:

```json
{"clientId":"manager-directory-reader","clientSecretFile":"/run/hermes/secrets/directory-client-secret"}
```

Use a dedicated confidential service-account client in the installation's realm
with only user query/view permissions (for example `realm-management`
`query-users` and `view-users`), never user- or role-management privileges. The
adapter uses client credentials over verified HTTPS; `hermesManager.oidcPrivateCa`
also supplies private-CA trust. Token and users endpoints derive from the configured issuer.
Use public subjects so the provider user ID matches the OIDC subject. See
[Keycloak's Admin REST API](https://www.keycloak.org/docs-api/latest/rest-api/index.html#_users).

Users → Keycloak directory has URL-backed search and query-bound pages; Keycloak
controls matching and order. It shows canonical issuer/subject, username, display
name, email and provider status, and can preselect an enabled identity for
assignment creation. Directory verification means a provider record was read; it
does not create an assignment or grant administration. Results stay in memory.
When unconfigured the view says so; provider denial or outage shows unavailable
rather than an empty directory. No Keycloak writes are implemented. Rotate the
Secret and restart the API role to reload the service credential.

## Implementation conventions

| Layer | Technology | Responsibility |
| --- | --- | --- |
| Application | React, TypeScript, Vite | Typed components; Vite is a build/development tool only |
| Navigation | TanStack Router (code-based routes) | Typed detail routes and validated URL search state |
| Tables and server data | TanStack Table, TanStack Query | Headless table state; Rust performs collection queries; in-memory caching and invalidation |
| Forms and editors | React Hook Form, CodeMirror 6 (JSON and YAML), pinned `yaml` parser | One canonical draft per editing session; editors are views of it |
| Validation | Versioned JSON Schema, precompiled Ajv; Rust `schemars` and `jsonschema` | Browser aid plus authoritative server validation |
| Primitives and style | Radix Dialog, repository CSS tokens | Focus and keyboard behavior; existing visual design |
| API contract | OpenAPI plus `openapi-typescript` | Generated client types from `contracts/hermes-manager/openapi.yaml` |
| Tests | Vitest, Playwright, Rust HTTPS/PostgreSQL fixtures | Client, browser and authorization coverage |

Installed versions are recorded in `ui/package.json`, `ui/package-lock.json`,
`Cargo.lock` and `rust-toolchain.toml`.

- **API boundary.** The UI calls relative `/api/v1/...` URLs and Rust auth routes
  through `src/api.ts`, and handles `401`, `403`, `409`, `412`, validation and
  dependency errors explicitly. Rust decides which paths receive SPA HTML; a new
  client route needs a matching server navigation route.
- **State.** Query keys include the resource, canonical query inputs and the
  active administrator. Logout, identity switch or lost permission clears all
  user-specific queries and drafts. Nothing is persisted in browser storage,
  URLs or bundles beyond the query state.
- **Mutations.** Each uses the current ETag and a stable idempotency key per
  intent. Conflicts show the newer record and keep the unsaved draft. The UI
  separates an acknowledged command from its eventual controller result.
- **Assets.** The loader accepts only startup-loaded HTML and flat JS/CSS: at
  most 32 files, 4 MiB each and 16 MiB in total. Dependency licenses ship in
  `/usr/share/doc/hermes-manager/THIRD_PARTY_NOTICES.txt`.
- **Limits.** Administration request bodies are bounded to 16 KiB and
  `adminConfig` to 8 KiB of serialized JSON; changing a limit needs a coordinated
  API, schema and controller review.
- **Accessibility.** Semantic tables, accessible names, visible focus, keyboard
  access, text alongside color for status, and dialogs that return focus.
- **Sources.** `ui/src/` holds `app/`, `api/`, `generated/` (never edited by
  hand), `components/`, `features/` and `schemas/`. Every UI input is listed in
  `release-policy.json`, and a check rejects tracked UI files missing from it.

## Frontend development and checks

The dashboard lives in `services/hermes-manager/ui`. Node and npm versions are
pinned by the image and frontend configuration:

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

CI rejects stale generated types and schema validators, and checks the
TypeScript build, browser workflows, Rust OIDC/PostgreSQL-grant/asset
authorization and the compiled process. Browser workflow tests use a synthetic
API fixture (`e2e/fixtures.ts`); Rust protocol tests use real HTTPS and
PostgreSQL with a signed OIDC provider fixture, and check every response
against the contract. The production browser test runs the built assets under
the real authentication, CSRF, grants and CSP:

```bash
cd services/hermes-manager
MANAGER_UI_DIST="$PWD/ui/dist" DATABASE_URL=postgres://hermes_test@127.0.0.1:5432/hermes_test \
  cargo test -p hermes-http --test dashboard_browser --locked -- --ignored
```

These fixtures do not provision a deployed Keycloak realm. The presentation
tests mock the API; the production case injects session cookies created by the
Rust fixture. They do not prove a real browser login-to-commissioning journey
through the installed chart. The [testing strategy](../TESTING.md)
requires Python/pytest browser E2E on k3d while preserving these frontend
unit/integration tests. See [G13 in the gap register](../testing/gaps.md#g13--real-browser-and-native-workflow-coverage)
for the missing browser scenarios.

`npm run dev` starts the Vite development server. For authenticated hot reload,
serve it over HTTPS with `MANAGER_DEV_TLS_CERT_FILE` and `MANAGER_DEV_TLS_KEY_FILE`
from outside the checkout. Set `MANAGER_DEV_ORIGIN` to the Rust administration
listener and `NODE_EXTRA_CA_CERTS` for a private CA. Set the manager browser
`publicOrigin` to the development origin (for example
`https://hermes-admin.localhost:5173`) and register its exact callback in the
development Keycloak client. Vite proxies `/api` and `/auth` to Rust, which still
performs every authorization check. No client secret belongs in a `VITE_`
variable or UI file.
