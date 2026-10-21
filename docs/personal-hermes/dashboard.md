# Manager dashboard scaffold

The manager now serves a React/TypeScript dashboard built by Vite. It includes
an overview, the first page of instance assignments, approved terminal runtimes
and administrator audit events. The lists use the actual administration API.
Assignment creation, lifecycle controls, template/policy editors, directory
search and pagination controls remain future work. There are no mock production
users, credentials or runtime records in the frontend.

The [dashboard feature specification](dashboard-features.md) details the next
searchable data views, template inspection, form/raw configuration editors,
per-template managed Hermes `adminConfig`, user assignments and editable
administrator list. It explicitly separates those requirements from the current
Keycloak role gate and records the administrator-authority decision still needed.

The [dashboard stack and implementation conventions](dashboard-stack.md) records
the current React/TypeScript/Vite/Rust foundation and planned TanStack, form,
Monaco, JSON Schema and Radix integrations. It covers data flow, source layout,
validation, protected workers/assets, accessibility and delivery checks; planned
libraries are distinguished from installed dependencies.

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

## Keycloak administrator access

Use a dedicated Keycloak OIDC client, for example `manager-admin`, on a separate
administration hostname from personal Hermes. Configure:

- An exact HTTPS callback: `https://hermes-admin.example.org/auth/oidc/callback`.
- The standard authorization-code flow with S256 PKCE, public subject identifiers
  and the client secret file if using a confidential client.
- An access-token audience mapper for `manager-api` and the configured authorized
  client ID. Set `admin.audience`, `allowedClients` and browser `clientId` to match.
- A client role such as `platform-administrator` under the configured
  `admin.managementClient`. Grant it only to platform administrators; configure
  its name in `admin.administratorRole`.
- The exact realm issuer and its HTTPS JWKS endpoint, plus a CA file if needed.

Today access is granted by that Keycloak client role, not by an administrator
allowlist in the manager database. PostgreSQL assignment rows control who gets
a Hermes instance. They do not grant dashboard access. An editable administrator
list is a new requirement; its authority/write path is not implemented. See the
[authorization design options](dashboard-features.md#administrator-list-and-authorization-authority).

Relevant access-token claims for the example configuration include:

```json
{
  "aud": "manager-api",
  "azp": "manager-admin",
  "resource_access": {
    "manager-admin": { "roles": ["platform-administrator"] }
  }
}
```

This is only a claims excerpt; the manager also validates the signature, issuer,
subject, token type, user-session evidence and validity times. Assign the client
role to the intended users in Keycloak and ensure its role mapper/scopes include
it in access tokens. This uses Keycloak's standard
[client-role token mapping](https://www.keycloak.org/docs/latest/server_admin/index.html#_role_mappings_in_the_token),
not an arbitrary unsigned `admin=true` flag. Granting/revoking a role affects
newly issued/refreshed tokens; current browser sessions refresh their verified
token after 60 seconds on use (or earlier near expiry). Already-issued bearer
tokens retain their claims until expiry; do not promise immediate revocation.

Anonymous page navigation redirects through `/auth/login`. Rust validates OIDC
state, nonce, PKCE, signature, issuer, audience, client and administrator role.
A personal Hermes assignment or a general realm role grants no manager access.
HTML and JS/CSS assets require the same server-side administrator authorization
as the API. Unknown API/auth/internal paths return errors rather than SPA HTML.

Access/refresh tokens remain encrypted server-side; the browser receives opaque
Secure, HttpOnly, host-only session cookies. The UI obtains actor/permissions and
a CSRF token from `/api/v1/session`; sign-out uses `POST /auth/logout` with CSRF
and exact-origin validation. Session expiry/revocation and role changes are
rechecked through the existing browser-session policy. Logout revokes the manager
session and opens a signed-out landing page without immediately restarting SSO;
it does not promise global Keycloak logout. UI responses use `no-store`,
`nosniff`, `no-referrer` and a restrictive same-origin content security policy.

## Frontend development and checks

Use Node 26.11.1 and npm 11.20.0, pinned by the image and frontend configuration:

```bash
cd services/hermes-manager/ui
npm ci --ignore-scripts
npm run check:generated
npm run format:check
npm test
npm run build
npx --no-install playwright install --with-deps chromium
npm run test:e2e
```

API types are generated from the repository's administration OpenAPI contract.
CI rejects stale generated types and checks the TypeScript/build, browser
presentation, Rust OIDC/role/asset authorization, compiled process and full
multi-stage image pipeline. The browser presentation tests use synthetic API
fixtures; Rust protocol tests use real HTTPS and PostgreSQL with a signed OIDC
provider fixture. These are not a deployed Keycloak realm/browser acceptance.

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
