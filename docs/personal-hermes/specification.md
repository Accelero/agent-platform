# Personal Hermes specification

Status: proposed requirements for a future release. Release 0.6.1 does not
implement these services, APIs or values. The [architecture](architecture.md)
selects the component design; the [plan](plan.md) defines delivery gates.

## Scope and terminology

The manager commissions one personal Hermes instance for each configured user.
The router exposes a common Hermes URL and routes authenticated requests to
that user's instance. The manager includes the first administration UI, behind a
separate versioned API so presentation can later move into its own service.

The new manager and OIDC authentication adapter are Rust services. The dashboard
uses React/TypeScript with Vite, served by the Rust manager. The proxy transport
is Traefik Proxy with a manager-querying middleware plugin after authentication
and before child-route selection. Adoption requires proving the session and
active-connection revocation requirements.
This is OIDC-based backend selection, not merely OIDC protecting a fixed backend.

| Term | Meaning |
| --- | --- |
| Identity | Exact configured OIDC issuer and verified subject; email is descriptive |
| Assignment | Manager-owned authorization and desired instance configuration for an identity |
| Instance | Stable platform ID, private namespace, native Hermes state and approved execution resources |
| Platform administrator | Actor with the configured management-client permission, distinct from a personal Hermes owner |
| Execution manager | Existing `runtime-manager`, responsible for scoped execution and leases rather than user assignment |
| Installation | Durable platform ownership identity shared by registry, resources and recovery manifests |

## Required behavior

| ID | Requirement |
| --- | --- |
| P01 | Keycloak remains the source of identities and authentication. The manager database is the source of personal-instance assignments. |
| P02 | An administrator must explicitly enable an assignment before commissioning or routing. Login or a group claim must not self-enroll a user. |
| P03 | An enabled assignment must enqueue proactive commissioning. Concurrent commands/logins/restarts must converge on one instance for that identity. |
| P04 | Every instance must have independent durable home/state, native session keys, model authentication and approved execution credentials. |
| P05 | One common Hermes hostname must route using the verified identity and current manager mapping. A missing, disabled, unknown or foreign mapping must never fall back. |
| P06 | Native chat, settings, history, uploads, HTTP mutations and WebSockets must remain supported with native authentication enabled. |
| P07 | The built-in administration UI must use the administration API and have no direct persistence, directory or Kubernetes access. API-only mode must remain usable. |
| P08 | Reconciliation and seeding must preserve live settings, profiles, memory, sessions and user-created agents. |
| P09 | Revocation must deny access and stop owned execution through a bounded lifecycle, retaining data until a separate purge decision. |
| P10 | Backup/restore must preserve registry ownership and instance data together, with fail-closed recovery and teardown gates. |
| P11 | New assignments must not inherit a board owner's enrollment, model login, API keys, private prompts or service privileges. |
| P12 | The verified OIDC `(issuer, subject)` must determine backend selection through the manager assignment. The same hostname and path must reach different backends for different configured owners. |
| P13 | A thin Traefik plugin must query the Rust manager's internal resolver for each HTTP request and WebSocket opening handshake. It has no direct SQL access and cannot create arbitrary proxy destinations. |

## Assignment administration

The built-in interface must provide paginated identity/assignment lists, optional
directory search, exact subject selection, template selection, enable, suspend,
revoke, status/conditions and retry. Show the selected canonical issuer/subject
before creation. Email-only creation and automatic email-based account linking
are invalid. When manual subject entry is used, show whether the directory has
verified the subject; a later successful login still has to match it exactly.

Administration requires an explicitly configured management role and client
audience. A valid personal login, Hermes assignment, general realm membership or
router service certificate does not confer administration. Every mutation must
authorize the actor on the server, check CSRF for browser-cookie requests, apply
an optimistic version and create an audit event in the same transaction.

Administrators can manage lifecycle metadata. The interface must not expose
model keys, native tokens, chat histories, private files, arbitrary Pod specs,
or an impersonation button. Optional directory access is read-only; identity
creation, password reset and identity-provider role administration remain in the
identity provider.

The management UI must be served from a separate origin from personal Hermes.
Disabling UI serving must leave the same versioned API, authorization and audit
behavior available to an external administration client.

The React/TypeScript dashboard uses HTTPS/JSON requests to the Rust API. Only
Rust services access PostgreSQL or reconcile Kubernetes. Vite produces static
assets during the locked frontend build; the production manager serves them
without a Vite or Node.js server. Generate the frontend API client from the
administration contract, keep tokens server-side and enforce authorization/CSRF
in Rust. No browser bundle may contain database credentials or live user data.

## Durable data contract

Use versioned PostgreSQL migrations and explicit constraints, at least:

| Record | Required contents and constraints |
| --- | --- |
| Installation | Stable ID, schema/template versions, bootstrap/recovery mode and maintenance epoch |
| Identity | Stable ID, exact issuer and subject, optional display/email cache; unique `(issuer, subject)` |
| Assignment | Installation, identity, desired access/state, approved template ID/version, resource/storage policy, optimistic version; unique `(installation_id, identity_id)` |
| Instance | Stable random ID, assignment ID, desired/observed generation, namespace name/UID, readiness conditions; unique assignment ID |
| Owned resource | Instance ID, kind, namespace, name, observed UID, purpose and retention disposition; reject foreign UID adoption |
| Operation/outbox | Type, instance, desired generation, idempotency key/body digest, lease owner/expiry, fencing version, bounded retry status |
| Audit event | Immutable actor issuer/subject or authenticated service ID, action, target, timestamp, request ID and redacted change metadata |
| Checkpoint | Installation/maintenance epoch, inventory schema/digest, participating instances, completion and verified recovery receipt |

Router/admin session stores use separate schemas or database roles from
assignment writes. Encrypt stored provider tokens using deployment-supplied key
references. Never put credentials in assignment API payloads, audit events or
database URLs in logs.

Issuers and subjects are case-sensitive validated identity strings, not mutable
display fields. The first version supports one configured issuer. It requires
compatible public subjects across router and native OIDC clients. A new subject
with an old email gets no access to the old subject's instance.

## Lifecycle and concurrency

Assignment access states are `enabled`, `suspended` and `revoked`. Only explicit
administrator commands can enable access. Suspension/revocation retain the
identity, instance ID and data. Re-enabling requires a new audited command;
directory synchronization cannot silently undo an administrator's revocation.

Instance observed states include `Pending`, `Provisioning`, `Ready`, `Blocked`,
`Suspending`, `Suspended`, `Retained`, `Deleting`, `Deleted` and `Error`.
Conditions distinguish storage, native auth, execution policy and model-provider
onboarding. `Ready` means the authenticated dashboard is usable; it does not
claim that an owner without model credentials can invoke a model.

Commissioning must reserve the instance ID durably before Kubernetes creation.
Render only approved templates. Persist ownership and verify native owner pin,
storage, network/admission policy and backend readiness before publishing a
route. The controller must tolerate interruption after any step.

Use a transactional outbox, unique constraints and leased/fenced work. Do not
hold SQL transactions across external calls. A stale worker cannot publish a
route or delete resources after losing its lease or desired generation. A
disable/create race must deny routing immediately, then reconcile owned partial
resources to the disabled state. Retry cannot change the identity or adopt a
same-named foreign object.

Permanent purge is distinct from revoke. It requires an authorized lifecycle
command, an explicit target/version/retention decision, a matching checkpoint
receipt or a separately explicit discard decision, and full ownership checks.
It is not part of the initial UI's normal disable operation. Never cascade-delete
homes because an identity disappeared from a directory response.

## API contracts

The implementation must supply OpenAPI and typed request/response contracts.
Endpoints below are proposed API v1, not routes supported by release 0.6.1.

| Boundary | Proposed endpoints | Caller and behavior |
| --- | --- | --- |
| Administration session | `GET /api/v1/session` | Current administrator actor, permitted actions and browser CSRF token; no provider tokens |
| Administration | `GET /api/v1/identities`, `GET /api/v1/assignments` | Authorized administrator; bounded pagination and optional read-only lookup |
| Administration | `GET /api/v1/templates` | Authorized administrator; approved template IDs, versions and policy summaries, without manifests or secrets |
| Administration | `POST /api/v1/assignments` | Create exact identity assignment; require `Idempotency-Key`; return `202` with operation and assignment |
| Administration | `GET/PATCH /api/v1/assignments/{id}` | Inspect/update permitted fields; PATCH requires `If-Match`; identity and instance IDs are immutable |
| Administration | `GET /api/v1/instances/{id}`, `POST /api/v1/instances/{id}/reconcile` | Inspect owned metadata or enqueue bounded retry; cannot supply workload manifests |
| Administration | `GET /api/v1/operations/{id}`, `GET /api/v1/audit-events` | Inspect progress and redacted audit with administrator permission |
| Router internal | `POST /internal/v1/routes/resolve` | mTLS Traefik plugin identity plus fresh audience-bound user access token; exact verified issuer/subject; no assignment mutation |
| Checkpoint internal | `POST /internal/v1/checkpoints`, `GET /internal/v1/checkpoints/{id}/inventory`, `POST /internal/v1/checkpoints/{id}/complete`, `POST /internal/v1/checkpoints/{id}/resume` | Dedicated coordinator identity; fenced lifecycle, never available to router/browser roles |
| Owner at router | `GET /_platform/me`, `GET /_platform/status` | Current authenticated owner's status only, with no arbitrary owner ID parameter |
| Platform auth at router | `/_platform/login`, `/_platform/oidc/callback`, `POST /_platform/logout` | Router OIDC/session lifecycle; separate from native Hermes callbacks |
| Native proxied API | Existing Hermes routes, including `/auth/callback`, `/auth/logout`, `/api/auth/ws-ticket` and `/api/ws` | Valid platform session and current route; native authentication remains mandatory |

An assignment creation body has this shape:

```json
{
  "identity": {
    "issuer": "https://sso.example.org/realms/agents",
    "subject": "00000000-0000-4000-8000-000000000001"
  },
  "access": "enabled",
  "templateId": "personal-default"
}
```

Repeated idempotency keys with the same actor/operation and body return the
original result. A changed body returns `409`. Stale `If-Match` returns `412`.
Malformed/unapproved configuration returns `422`; unauthenticated requests use
`401` and insufficient permission uses `403`. Browser navigation may redirect to
login, but API mutations must not be replayed across an authentication redirect.
Responses and errors must not expose another owner's data or credentials.

The internal resolver returns `ready`, `pending`, `denied` or `unavailable` with
bounded reason codes. A ready result contains a stable instance ID, registered
namespace/Service/port, observed route generation and authorization version.
It cannot contain a caller-provided upstream URL. The router maps pending to an
owner-only `202` status view, denied to `403`, and unavailable to `503`.

The proposed [resolver OpenAPI contract](../../contracts/hermes-manager/router.openapi.yaml)
defines this endpoint, including response bodies and HTTP statuses. Both service
mTLS and a Keycloak access token for the configured route-API audience are
required. Restrict the token's authorized client to the router client and match
its verified issuer/subject to the request. Enforce the identity-evidence age
bound below. A service certificate alone cannot assert a user's identity.

Ready responses identify the registered destination and its instance ID, route
generation and authorization version. The plugin sets the instance/generation
headers used by child routing only after validating that response. Denied,
pending, malformed, stale-evidence and unavailable responses supply no destination.
Use `Cache-Control: no-store`, a 2-second total deadline, bounded request/response
sizes and no token, cookie or request-body logging for this internal boundary.

The administration API accepts either its own browser session with CSRF checks
or a validated administrator access token for its configured audience. A future
panel can use the latter. Service authentication never supplies a human actor's
permission by assertion alone. Do not expose assignment database credentials to
the panel.

## Routing, native auth and session requirements

The router must validate the platform identity before selecting a destination.
Every HTTP request resolves current assignment authorization; initially do not
cache positive assignment decisions. Browser path, host, email, headers, cookie
contents and callback parameters cannot independently select an instance.

Derive identity from a validated OIDC session, including the verified issuer and
subject. Decoding JWT claims without signature/issuer/audience validation is
invalid. Manager assignments authorize that identity's backend; they do not
replace OIDC authentication or allow a caller to nominate another subject.

For Traefik, the parent router must run Rust OIDC/ForwardAuth validation, then
the manager-querying plugin, before child selection. Remove client-supplied
`X-Hermes-Instance`, `X-Hermes-Route-Generation` and internal identity/evidence
headers before validation. Only the plugin may supply resolved routing headers,
after a ready manager decision. Children map the exact instance/generation to
registered Services and cannot be reached through a separate public entry point.
Strip internal identity, evidence and decision headers before native forwarding.
A failed or unavailable validator, plugin or manager cannot yield a route.

Implement the thin plugin in Go/Yaegi using HTTP/mTLS to the manager, without SQL
drivers or duplicated assignment rules. Pin and package its reviewed source with
the platform release and load it at proxy startup. Keep the manager and OIDC
component in Rust. A Rust/WASM plugin is an optional later implementation only
after proving the required outbound networking and middleware compatibility.

Generate hierarchical routes through the supported file provider using atomic,
validated snapshots. Stale route configuration cannot override a revoked
assignment. The proxy has no Kubernetes API credential, and neither the browser
nor the UI can publish destinations or routing configuration.

The native provider must independently check the expected instance owner after
OIDC verification, including refresh and existing-session verification. Keep
state/nonce/PKCE, audience, issuer, key and algorithm validation. A globally
permitted identity trying another instance must still fail this native check.

The router and native authorization-code flows have distinct callbacks and
state. Forward a native callback only with a current platform session resolving
to that same owner. Expired/switched sessions abort stale callbacks. Do not
decode an unverified token to pick the callback destination.

Platform cookies must be opaque, Secure, HttpOnly, host-only and SameSite
compatible with the tested redirect flow. Keep platform tokens server-side.
Do not send platform/admin cookies or platform bearer tokens upstream. Strip
untrusted identity/forwarding headers and reject backend attempts to set reserved
platform cookies. Native keys and cookies must not authenticate another instance.

Require origin checks and native single-use WebSocket tickets. Authenticate and
resolve ownership before upgrading, preserve full duplex streaming and repeat
authorization for the established connection. Do not log ticket query strings,
OIDC codes, bearer tokens, refresh tokens, cookies or private chat content.

The middleware lookup occurs per HTTP request and once per WebSocket handshake,
including reconnects. WebSocket frames do not re-enter the HTTP middleware.
Periodic established-connection authorization is a separate mechanism that must
meet the bounds below; a plugin at handshake time does not implement it alone.

On logout, invalidate the platform session and expire native cookies even if
the backend cannot be reached. Identity switching must invalidate old callbacks
and prevent old native cookies or tickets reaching another identity's instance.

Proposed initial operational bounds, configurable only within tested limits:

| Setting | Initial requirement |
| --- | --- |
| Route resolution | 2-second total deadline; failure denies the request |
| Assignment revocation | Next HTTP request denied after transaction commits; open WebSockets closed within 10 seconds |
| WebSocket authorization | Recheck every 5 seconds; manager failure closes within the same 10-second bound |
| Identity evidence | Refresh at most every 60 seconds; stop accepting evidence older than 120 seconds if refresh/directory verification fails |
| External identity revocation | Effective within 130 seconds, including existing WebSockets; prove against configured Keycloak session behavior |
| Upload size | 64 MiB maximum unless an explicitly tested deployment policy lowers it |
| WebSocket/stream idle | Up to 1 hour with native heartbeat support and resource/connection limits |
| Controller calls | Explicit connection/read deadlines, bounded work concurrency, capped retry backoff and cancellation |
| Disable drain | At most 5 minutes for owned work, followed by controlled stop; access is already denied |

Identity refresh must re-evaluate provider status/claims; checking an old JWT's
signature repeatedly does not meet the revocation bound. A router/manager outage
must not extend cached authorization indefinitely. Health probes are separate
minimal endpoints and do not reveal user inventories.

The Traefik compatibility test must independently prove active-connection
revocation. ForwardAuth at handshake time and configuration reloads alone are not
accepted as proof that an existing WebSocket meets these bounds. Proxy adoption
remains blocked until that mechanism is demonstrated.

## Kubernetes and storage requirements

The manager API/UI and router have `automountServiceAccountToken: false`. Only
the controller gets the constrained namespace-bootstrap permissions. Instance
RoleBindings grant reconciliation access only inside registered namespaces;
fail-closed admission must reject other names, installations, roles or subjects.
Do not give the controller cluster-admin, impersonation, unrestricted role
binding, or a cluster-wide permission to read Secrets.

Each instance has its own retained home/workspace claims, session keys,
ServiceAccounts and execution credentials. Enforce restricted security contexts,
selected volume/Secret references, non-root execution, resource limits and
default-deny network policies. Namespaces and labels are identifiers, not a
substitute for tested RBAC, admission and network enforcement.

Execution workers can reference only that owner's approved template, profile,
PVCs, Secret keys and services. A shared manager credential or global mutable
home is invalid. The existing execution manager remains a separate component;
its per-instance static configuration must preserve current lease and task
isolation behavior. Unsupported storage sharing/scheduling must block readiness.

External service credentials are opt-in and scoped to the assigned owner. An
optional deployment adapter may return approved Secret references; the default
grants no external service access. Copying the original owner's MLflow, SkyPilot
or other shared automation identity into every instance is invalid.

## Recovery and installation contract

Require explicit fresh, restore or existing mode. Fresh mode refuses an
unexplained existing registry or owned instance data. Restore mode fences routing
and commissioning until a coordinated recovery receipt has been verified.
Existing mode requires matching installation identity and supported schema.

The versioned inventory must cover registry/assignments, owner keys, instance IDs,
namespaces and resource UIDs, selected PVC datasets and Secrets, native settings
and authentication state, execution-manager state, template versions and image
digests. Inventory generation must fence both administrator changes and
reconciliation; the coordinator must stop all writers before taking its payload.

Encrypted full-payload readback and a matching inventory/epoch receipt are
mandatory before final teardown. Failed/incomplete snapshots do not become the
latest completed recovery point. Restore must bind new Kubernetes UIDs explicitly
while preserving stable OIDC ownership and instance IDs. Missing registry data
alongside retained homes must block automatic provisioning.

The platform provides lifecycle/inventory contracts and generic validation.
External storage, encryption keys, transport and deployment-specific backup Jobs
remain deployment-owned. Personal mode must not be adopted by a consumer whose
checkpoint integration still backs up only a static singleton dataset list.

Proposed chart configuration groups are `hermesInstances`, `hermesManager` and
`hermesRouter`. They must cover enable/mode, existing database and Secret refs,
OIDC clients/administrator permission, template/storage/capacity policy, optional
directory reader, internal mTLS, recovery gate and router/admin ingress. These
names are a design contract to implement, not installable 0.6.1 values. Do not put
per-user assignments or inline model credentials into Helm values. Bootstrap
administrators are identity-provider roles, not a second user-password database.

## Acceptance matrix

| Test | Required result |
| --- | --- |
| Two configured identities, one common URL | Distinct instances, settings, sessions, uploads and native owner checks |
| Same hostname/path, different verified OIDC subjects | Each request reaches its assigned backend; changing browser headers, email or unverified JWT claims cannot select another backend |
| Valid but unassigned identity | Denied; no commissioning side effect |
| Concurrent enable/login/retry and controller crash | One stable instance; interrupted work reconciles safely |
| Email rename or recreated email under a new subject | Original ownership unchanged; new subject cannot inherit data |
| Forged headers, foreign cookie/ticket, callback race or backend target | No cross-user route, session or credential access |
| Native HTTP and WebSocket through router | OIDC callback, mutations, upload, ticket, chat readiness/heartbeat and logout succeed |
| Manager/database or identity-provider outage | Fail closed within stated bounds; no shared-instance fallback |
| Suspend/revoke during an open chat or provisioning | New access denied, connection closed within bound, data retained |
| Malicious worker/namespace bootstrap request | Cannot access another owner, bind into a foreign namespace or mount disallowed Secrets/PVCs |
| New owner without provider authentication | Private onboarding is available; no inherited operator model login |
| Built-in UI disabled and external API client used | Same assignment workflow and permissions without SQL/Kubernetes access |
| Frontend build and browser requests | Static React/TypeScript build served by Rust; only the administration API is called, and browser bundles contain no credentials or live identities |
| Plugin resolver boundary | Valid mTLS plus exact fresh identity resolves a registered destination; forged headers, wrong audience/client, malformed responses and manager failure deny routing |
| UI import/dependency boundary | Presentation cannot import database, directory or Kubernetes adapters |
| Recovery with registry and both users' data | Stable owners/instance IDs restored, new UID bindings verified, stale sessions invalidated |
| Missing registry, partial snapshot or wrong installation receipt | Provisioning/teardown blocked; no automatic adoption or replacement |
| Existing singleton adoption | Explicit owner binding and checked storage migration; original board connection is not copied to other people |

Run isolated contract/domain tests, real PostgreSQL concurrency/migration tests
and disposable local Kubernetes/OIDC/browser tests as appropriate. Generic
acceptance uses fixture identities and model stubs; it must not require a paid
GPU, private model account, public invitation or destructive live stack. Record
actual results and untested cases before a release claims these requirements.
