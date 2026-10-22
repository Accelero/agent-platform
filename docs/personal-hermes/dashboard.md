# Manager dashboard

The manager serves an authenticated React/TypeScript administration dashboard
from its Rust API role. Administrators use it to manage assignments, Hermes
templates, terminal runtimes, storage bindings, activity and PostgreSQL
administrator grants. Keycloak supplies verified identity; PostgreSQL grants are
the sole authority for administrator access. Infrastructure-owned templates and
assignments stay read-only in the dashboard.

## Views

Every collection uses server-side search, filters, sorting and bounded pages,
with the query in the URL. Changing the query returns to the first page.
Loading, empty, denied, unavailable and stale states are shown separately; a
failed query never looks like an empty database.

| View | Shows | Actions |
| --- | --- | --- |
| Users | Exact issuer/subject, optional labels, provenance, current assignment and instance state; optional live Keycloak directory tab | Create an assignment for an exact identity, or open the existing one |
| Hermes templates | Template ID, version, image digest, source/ownership, draft/published state, assignment count | Inspect, compare versions, clone, draft, validate, publish and apply |
| Terminal runtimes | Runtime ID, immutable versions, image, SSH contract, resources, storage summaries | Inspect a version, see which assignments may request it, create the next version |
| Assignments | Identity, template version, access, desired/observed generation, conditions | Change template, access or execution policy; retry; inspect operation history |
| Administrators | Exact identities with PostgreSQL grants and audit metadata | Grant, demote or revoke access (with `canManageAdministrators`) |
| Activity | Actor, action, affected records, outcome and time | Open related records and redacted change details |

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
builds both manager executables with the Cargo lockfile. The final Debian image
runs as UID/GID 10001 with the binaries, CA certificates, SSH client and static
UI, and no Node/Vite server. Only files listed under
`development_contexts.hermes-manager` in `release-policy.json` enter the build.
Build diagnostics are bounded in `.local/hermes-manager-build.log`. The same
input set is the `hermes-manager` release target, and the chart installs it; see
the [chart guide](../../charts/agent-platform/README.md#rust-manager-and-dashboard).

Use [the API configuration example](../../services/hermes-manager/api.example.json)
as a nonsecret shape reference. The chart sets `admin.uiDirectory` to
`/usr/share/hermes-manager/ui` when browser sessions are configured; when
running the binary locally, point it at the absolute `ui/dist` path. Supply the
installation, PostgreSQL, TLS and browser-session inputs through private mounted
files, and initialize or migrate the registry before starting the API role:

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
permission. The `managementClient` and `administratorRole` configuration fields
are accepted and ignored by authorization. No Keycloak role-management
credentials are needed.

Migration 0022 creates installation-scoped grants keyed by exact issuer/subject.
It creates no implicit administrators. After migrating, bootstrap the first
administrator. Put a verified identity in a private file outside the checkout
and build context:

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
action, require database/operator access and never print the identity or accept
it as a command-line argument.

An active grant permits administration; `canManageAdministrators` additionally
permits grant changes. Grant targets must have verified login evidence. Grant
edits use ETags, actor-scoped idempotency, audit and transactional protection
against removing or demoting the last administrator who can manage grants.

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

Open a user to create an assignment for that exact identity or manage the
existing one. Creation reviews the selected immutable template, managed overrides
and enforced platform settings before submission.

Assignment details show desired and observed template versions, desired and
verified applied execution configurations, and paged operation history. Activity
provides redacted change details; execution configuration events say Recorded,
acknowledging the configuration write rather than completed reconciliation.
Journal payloads and private user configuration are never exposed.

Terminal runtime details show the immutable definition and version history and
the assignments allowed to request each version. Runtime and storage-binding
selectors use searchable manager references scoped to the selected instance's
namespace and UID. Storage-class suggestions come from published templates;
Kubernetes availability is checked during reconciliation.

## Templates, publication and rollout

A Hermes template holds deployment inputs and its managed Hermes configuration:

```json
{
  "schemaVersion": 1,
  "image": "registry.example.org/platform/hermes@sha256:<64 lowercase hex digits>",
  "storageClass": "local-path",
  "homeGib": 5,
  "workspaceGib": 10,
  "cpuMillis": 1000,
  "memoryMib": 2048,
  "adminConfig": {
    "agent": { "max_turns": 30 }
  }
}
```

`adminConfig` holds administrator-enforced Hermes settings for every assigned
user; other native settings stay on the user's home PVC and editable in Hermes.
The managed configuration preview distinguishes three layers:

1. Template overrides in `adminConfig`, such as a turn limit or a pinned
   default `terminal.runtime_id`.
2. Platform-enforced settings: `terminal.backend=managed_execution`, the
   image-owned plugin and protected connection paths. Neither editor can disable
   them, and no credential value is shown.
3. Settings left to the user. Removing an override can expose the user's
   retained value again; it never erases their config.

Choosing a default runtime in a template does not grant backend permission; the
assignment's execution allowlist decides. `adminConfig` must not contain model
keys, passwords, personal prompts or other private seed data.

Manager drafts live in dedicated authoring tables, never in infrastructure
desired-state rows. Infrastructure definitions can be inspected with redaction
and diagnostics but not edited or adopted. Publishing validates a draft and
creates an immutable catalog version; it never updates existing assignments.
Review the semantic diff, select an exact version and apply it explicitly.
Access, identity, instance, retained PVC data and native credentials stay with
the same owner.

Apply previews show resources, managed overrides, storage and restart effects.
Storage-class changes, shrinking and infrastructure source conflicts block
dispatch. Enabled instances drain execution and update through the native
Recreate path. Bulk apply covers only explicitly selected IDs and reviewed
versions (at most 50). Its durable result page separates dispatch acceptance
from each instance's operation, and the original administrator can resume
pending dispatch without reapplying accepted members. Polling backs off and
stops after ten minutes or a terminal result.

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

Optionally set `admin.directory` in the private API configuration:

```json
{"clientId":"manager-directory-reader","clientSecretFile":"/run/manager/secrets/directory-client-secret"}
```

Use a dedicated confidential service-account client in the installation's realm
with only user query/view permissions (for example `realm-management`
`query-users` and `view-users`), never user- or role-management privileges. The
adapter uses client credentials over verified HTTPS; `oidc.caFile` also supplies
private-CA trust. Token and users endpoints derive from the configured issuer.
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

Node and npm versions are pinned by the image and frontend configuration:

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

CI rejects stale generated types and checks the TypeScript build, browser
workflows, Rust OIDC/PostgreSQL-grant/asset authorization, the compiled process
and the full multi-stage image. Presentation tests use synthetic API fixtures;
Rust protocol tests use real HTTPS and PostgreSQL with a signed OIDC provider
fixture. The production browser test runs the built assets under Rust's real
authentication and CSP:

```bash
cd services/hermes-manager
MANAGER_UI_DIST="$PWD/ui/dist" DATABASE_URL=postgres://hermes_test@127.0.0.1:5432/hermes_test \
  cargo test -p hermes-adapters --test browser production_dashboard_browser_csp --locked -- --ignored
```

These fixtures do not provision a deployed Keycloak realm.

`npm run dev` starts the Vite development server. For authenticated hot reload,
serve it over HTTPS with `MANAGER_DEV_TLS_CERT_FILE` and `MANAGER_DEV_TLS_KEY_FILE`
from outside the checkout. Set `MANAGER_DEV_ORIGIN` to the Rust administration
listener and `NODE_EXTRA_CA_CERTS` for a private CA. Set the manager browser
`publicOrigin` to the development origin (for example
`https://hermes-admin.localhost:5173`) and register its exact callback in the
development Keycloak client. Vite proxies `/api` and `/auth` to Rust, which still
performs every authorization check. No client secret belongs in a `VITE_`
variable or UI file.
