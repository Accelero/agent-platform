# Personal Hermes specification

Status: target requirements for the personal Hermes platform, which is in
development and has no release yet. The [architecture](architecture.md) selects
the component design. These are target requirements rather than a feature
inventory. See the
[implementation status](implementation.md) for completed source behavior and
remaining gaps, and [native commissioning](native-commissioning.md) for the
implemented infrastructure-to-manager contract.

## Scope and terminology

One Rust manager commissions a personal Hermes instance and terminal workers for
each configured user, using PostgreSQL for assignments, runtime policy,
environments and leases.
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
| Platform administrator | Verified identity with an active installation-scoped PostgreSQL administrator grant, distinct from a personal Hermes owner |
| Execution module | Terminal provisioning and lease lifecycle inside the same Rust manager; no separate per-owner execution-manager service |
| Terminal runtime | Approved Kubernetes SSH worker image and execution policy, separate from the Hermes application image |
| Storage binding | Owner-bound reference to a namespace-local claim, with permitted mount paths, access and recovery policy |
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
| P14 | Each Hermes assignment must have independently configurable terminal image versions and mounts. Reusing an image must never reuse another owner's storage or credentials. |
| P15 | One Rust manager must administer user-to-instance assignments, serve route resolution and reconcile both Hermes instances and terminal worker Pods. Only its controller role receives elevated Kubernetes permissions. |
| P16 | PostgreSQL must durably model all manager assignments, runtime/storage configuration, execution clients, environments, leases and lifecycle operations. Personal mode must not depend on a separate execution-manager service, per-owner SQLite state or independent static execution configuration. |
| P17 | Each enabled, commissioned Hermes gateway must stay running at one desired replica for native cron and background integrations. HTTP inactivity must not trigger Hermes scale-to-zero; terminal worker lifetimes remain independently configurable. |
| P18 | Every acknowledged manager mutation and every external lifecycle action must have a durable PostgreSQL recovery record at the required commit boundary. After an ungraceful restart, reconciliation resumes committed work without duplicate instances, worker generations, lease grants or destructive replay. |

## Assignment administration

The built-in interface must provide paginated identity/assignment lists, optional
directory search, exact subject selection, template selection, enable, suspend,
revoke, status/conditions and retry. Show the selected canonical issuer/subject
before creation. Email-only creation and automatic email-based account linking
are invalid. When manual subject entry is used, show whether the directory has
verified the subject; a later successful login still has to match it exactly.

Keycloak supplies the verified identity and configured audience/client checks.
Administration requires an active PostgreSQL grant for that exact issuer/subject. A valid personal login, Hermes assignment, general realm membership or
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

Administrators must also manage the approved terminal-runtime catalog and each
assignment's default/allowed runtime versions, storage bindings, resources and
desired/effective policy status. Configure the Hermes application image through
its approved instance template independently of the terminal worker image.

The [dashboard guide](dashboard.md) expands this into
searchable/filterable data tables, template details and synchronized form/raw
editors, template-managed Hermes `adminConfig`, explicit user/template assignment
and an editable administrator list. PostgreSQL grants are the accepted and
implemented sole administrator authority; token roles are not prerequisites or
fallbacks. Grant management has separate permission, last-administrator protection,
operator bootstrap/recovery and current-grant checks for browser and bearer access.

## Durable data contract

Use versioned PostgreSQL migrations and explicit constraints, at least:

| Record | Required contents and constraints |
| --- | --- |
| Installation | Stable ID, schema/template versions, bootstrap/recovery mode and maintenance epoch |
| Identity | Stable ID, exact issuer and subject, optional display/email cache; unique `(issuer, subject)` |
| Assignment | Installation, identity, desired access/state, approved instance-template ID/version, default/allowed terminal-runtime versions, owner-specific mount/resource policy, optimistic version; unique `(installation_id, identity_id)` |
| Instance | Stable random ID, assignment ID, desired/observed generation, namespace name/UID, readiness conditions; unique assignment ID |
| Terminal-runtime version | Stable catalog ID and immutable version, exact OCI image digest, Kubernetes SSH contract, UID/GID, approved resources/environment/network policy and pull-Secret references |
| Storage binding | Installation/owner, registered namespace/PVC identity and backing-storage reference, exact permitted mounts/access, external ownership, required/optional status and backup/retention policy; no inline credentials |
| Effective execution policy | Instance, approved runtime versions and resolved owner mounts, desired/applied generation, content digest, activation/drain status and rendered worker/admission identities |
| Execution client | Stable ID, owning instance, credential hash/reference, allowed profiles, rotation/revocation state; cannot assert another owner or administer configuration |
| Execution environment/generation | Installation/instance, profile, runtime version, session or profile lifetime key, effective policy digest, desired/observed generation, namespace/Pod UID binding, readiness and drain/retirement state; unique active scoped environment identity |
| Execution request/lease | Client-scoped idempotency request/body digest and environment, lease ID, parent/delegate relationship, expiry/heartbeat/release state and fencing version; transactional uniqueness and exclusive-mode constraints |
| Owned resource | Instance ID, kind, namespace/name, reserved creation-operation identity, expected namespace UID, intended spec digest, observed resource UID, purpose and retention disposition; reject foreign UID adoption |
| Operation/outbox | Durable operation/target IDs, type, immutable policy/input references, desired generation, idempotency key/body digest, state/current step, lease owner/expiry, fencing version, next-attempt time and bounded retry status |
| Operation step | Operation/step key, committed action intent, expected UID/resource version and manifest digest, attempt/fencing token, observed result/UID, verification state and redacted failure; unique `(operation_id, step_key)` |
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

Enabled Hermes instances run continuously, including when no browser, HTTP
request or terminal lease is active. Maintain one desired application replica;
do not add an HTTP activator/autoscaler to stop or wake Hermes based on traffic.
Its native cron scheduler stays inside the running gateway. Terminal Pods may
be started, idled or retired independently by the unified manager's execution
module. Explicit suspend/revoke, maintenance, recovery and failure handling still
use the controlled manager lifecycle.

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

## Crash recovery and persistence boundaries

Recovery must work after abrupt process death without shutdown hooks. Use logged
PostgreSQL tables and durable commits; require `fsync`, `full_page_writes` and
`synchronous_commit` enabled for manager state. In-memory queues, timers, watch
caches and database notifications are accelerators, not recovery records.
Database migrations and startup checks must verify the expected schema,
installation identity and recovery/maintenance epoch before enabling operations.
These are manager process-recovery requirements; coordinated database/data loss
recovery follows the separate checkpoint contract.

| Boundary | Commit before action or response | Recovery and completion rule |
| --- | --- | --- |
| Assignment/configuration mutation | Desired state, version, idempotency result, audit event and outbox operation in one transaction before `202`/success | Retry with the same key returns the recorded result; a process crash cannot lose an acknowledged change or its pending work |
| Resource create/update | Stable target identity/name, pinned policy/spec digest, next step, operation fence and any encrypted bootstrap material before the Kubernetes call | Read the exact target and verify ownership, intent and applied spec; persist observed UID/result before advancing; an uncertain response is reconciled rather than blindly repeated |
| Native initialization | Initialization operation, seed/schema version and create-once credential intent before dispatch | Reuse the matching durable receipt or resume an idempotent initializer; never reseed an existing home or regenerate credentials merely because a response was lost |
| Execution lease grant/heartbeat/release | Scoped lease identity, environment/generation, expiry and request outcome committed before returning success | Lost-response retries reuse the committed live lease; expired/released leases remain terminal and cannot be revived; expiry uses database time |
| Resource retirement/delete | Retiring/tombstone state, exact resource UID, retention decision and required checkpoint receipt before deletion | Delete with UID/resource-version preconditions; observe absence of that resource before marking complete; an accepted delete is not completed cleanup |

Creation has a specific recovery window: Kubernetes may create an object before
the process records its UID. Before the call, persist a unique creation intent
and verifiable installation/instance/operation/generation marker bound to the
target and manifest digest. While the first UID is still unbound, recovery may
bind only the exact outstanding intent after verifying that marker, namespace
UID where applicable and intended resource fields. Namespace creation first
verifies installation/cluster scope and binds the namespace UID before any
child resource is created. Name/label matching alone is insufficient.
Once bound, a different UID blocks adoption or replacement until the explicit
lifecycle permits it. Generated bootstrap secrets must retain their original
encrypted material or verified reference across retries.

Track operation states such as `Pending`, `Running`, `Waiting`, `RetryAt`,
`Succeeded`, `Blocked` and `Failed`, with durable step outcomes and bounded retry
times. Claim work in short transactions, with database-time leases and increasing
fencing tokens. No SQL transaction may remain open across an external call.
Every result/state transition compares the operation fence, desired generation
and installation epoch. Stale workers cannot commit progress or renew ownership.
External updates/deletes also use Kubernetes UID/resource-version preconditions;
a database lease check alone does not make a Kubernetes write atomic with SQL.

After a timeout or lost response, record an unknown outcome, observe the target
and reconcile it before issuing the next action. Delivery is at least once;
operation identity, idempotent effects and verification provide convergence.
Do not claim exactly-once execution across PostgreSQL and Kubernetes. An
initializer or external action without a verifiable receipt/idempotency contract
must enter `Blocked` for review instead of being replayed automatically.

At startup, rebuild pending work and due timers from PostgreSQL, reclaim only
expired operation leases and relist registered owned Kubernetes resources.
Reconcile live UIDs/specs/readiness with pending steps, including actions that
succeeded before a crash. Preserve unexpired execution leases and their original
expiry; retire expired ones rather than extending them to restart time. Missing
or failed observations must not be treated as proof of absence. Periodic resync
must repair a missed watch event or lost notification without manual re-enqueue.

Database unavailability stops new grants and lifecycle writes; it must not cause
fresh-install fallback or a second local state store. Healthy Hermes/worker Pods
are not restarted merely because the manager restarted. Arbitrary terminal
commands, agent turns and cron side effects are not automatically replayed by
manager recovery; their application execution receipts remain authoritative.

## API contracts

The implementation must supply OpenAPI and typed request/response contracts.
Endpoints below are API v1. The OpenAPI files in `contracts/hermes-manager/`
are authoritative for the implemented subset.

| Boundary | Proposed endpoints | Caller and behavior |
| --- | --- | --- |
| Administration session | `GET /api/v1/session` | Current administrator actor, permitted actions and browser CSRF token; no provider tokens |
| Administration | `GET /api/v1/identities`, `GET /api/v1/assignments` | Authorized administrator; bounded pagination and optional read-only lookup |
| Administration | `GET /api/v1/templates` | Authorized administrator; approved template IDs, versions and policy summaries, without manifests or secrets |
| Administration | `GET/POST /api/v1/terminal-runtimes`, `GET /api/v1/terminal-runtimes/{id}/versions`, `POST /api/v1/terminal-runtimes/{id}/versions` | Inspect/register approved terminal definitions and immutable image/policy versions; mutations require idempotency and audit |
| Administration | `GET/POST /api/v1/storage-bindings`, `GET/PATCH /api/v1/storage-bindings/{id}` | Register owner-bound storage references and permitted access; no inline credentials, raw Pod/PV specs or arbitrary cross-namespace claims |
| Administration | `GET/PATCH /api/v1/assignments/{id}/execution` | Configure default/allowed terminal runtimes and mounts; inspect desired/effective policy and operation status; PATCH requires `If-Match` |
| Administration | `POST /api/v1/assignments` | Create exact identity assignment; require `Idempotency-Key`; return `202` with operation and assignment |
| Administration | `GET/PATCH /api/v1/assignments/{id}` | Inspect/update permitted fields; PATCH requires `If-Match`; identity and instance IDs are immutable |
| Administration | `GET /api/v1/instances/{id}`, `POST /api/v1/instances/{id}/reconcile` | Inspect owned metadata or enqueue bounded retry; cannot supply workload manifests |
| Administration | `GET /api/v1/instances/{id}/execution-environments` | Redacted environment/lease status and applied policy, without command content or credentials |
| Administration | `GET /api/v1/operations/{id}`, `GET /api/v1/audit-events` | Inspect progress and redacted audit with administrator permission |
| Router internal | `POST /internal/v1/routes/resolve` | mTLS Traefik plugin identity plus fresh audience-bound user access token; exact verified issuer/subject; no assignment mutation |
| Execution internal | `POST /execution/v1/leases/acquire` | Instance-scoped Hermes client; profile/session/lifetime, idempotency request and optional approved `runtime_id`, parent lease and bounded invocation context; image/mount/template specs are invalid |
| Execution internal | `POST /execution/v1/leases/heartbeat`, `POST /execution/v1/leases/release`, `POST /execution/v1/sessions/close`, `GET /execution/v1/environments` | Same scoped client; renew/release/close/inspect only its authorized execution state |
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

Execution routes are part of this Rust manager, on a restricted internal
listener/Service boundary. A per-instance credential determines instance scope
server-side and cannot call administration, routing or checkpoint endpoints.
Native Hermes can use a manager base URL ending in `/execution` with its current
`/v1/...` request paths. The Rust API must preserve validated native lease
payload/response semantics, while adding approved `runtime_id` selection.

An acquire request returns the ready environment/generation, lease ID/TTL and
SSH endpoint only after Pod readiness. Pending provisioning uses bounded `409`
retry with the same client-scoped request ID; dependencies failing use `503`.
No lease is active while the Pod is still provisioning. The database must
deduplicate retries and fence exclusive acquisitions. Heartbeat/release/session
close independently verify owner, environment and parent/delegate scope.
The lease-grant transaction must recheck current assignment access and effective
policy generation after provisioning. Reject new work during revoke/drain and
never revive a lease that expired before a heartbeat or parent-lease join.

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

Execution workers can reference only that owner's approved runtime, profile,
PVCs, Secret keys and services. A credential shared across instances or global
mutable home is invalid. The shared Rust controller handles worker lifecycle,
using exact instance/namespace/Pod UID bindings and the PostgreSQL policy.
Preserve existing lease/task isolation, idle/recycle and crash recovery behavior
without deploying a second execution manager. Unsupported storage sharing or
scheduling blocks the affected terminal runtime rather than changing its mounts.

External service credentials are opt-in and scoped to the assigned owner. An
optional deployment adapter may return approved Secret references; the default
grants no external service access. Copying the original owner's MLflow, SkyPilot
or other shared automation identity into every instance is invalid.

## Hermes application image, execution plugin and caller identity

This repository must build and supply the Hermes application image. Start from
the supported upstream Hermes distribution pinned by digest/revision, then apply
the reviewed platform modifications and bundle the plugin developed in this
repository. Record the upstream origin and preserve its license notices. Build
from the reviewed release inputs; do not download mutable plugin code at startup
or include personal profiles, credentials, prompts, memory or live state in the
image. Infrastructure selects the resulting immutable platform image digest in
its instance template; it does not need to build a personal image for each user.

The repository-owned `managed_execution` plugin is the Hermes integration for
on-demand terminal containers. When Hermes needs command execution, the plugin
requests an environment/lease from the shared manager over authenticated HTTPS.
The manager selects the permitted terminal image, storage and policy for that
Hermes instance and creates or reuses its worker. The plugin waits for a verified
ready lease, then uses the returned SSH endpoint for execution and file transfer.
It renews/releases leases and closes sessions according to the agreed protocol.
The plugin has no Kubernetes credential and cannot directly create containers,
choose arbitrary images/mounts or run a local execution fallback after denial.

### Authentication and instance identification

The manager must provision a distinct execution credential for each logical
Hermes instance and register its server-side binding before supplying it to the
application. Mount the credential read-only at runtime, keep it out of images,
ordinary configuration and logs, and store only its authentication hash in the
execution-client registry. Any raw bootstrap copy persisted in the manager
database must remain encrypted and scoped to the same instance. The plugin reads
this credential and authenticates every manager execution request over
certificate-verified TLS.
It does not forward the owner's browser cookie or Keycloak token as its service
identity.

The manager derives the caller's installation, instance, assignment/owner and
execution scope from the authenticated credential and its own registry. A
caller-supplied user, instance, namespace, Pod name, container ID or runtime name
cannot select another identity or increase permissions. Recheck current access,
installation epoch, approved runtime policy and lease ownership on requests;
disabled/revoked or foreign execution clients must fail closed. Execution
credentials grant no administration, routing or checkpoint access.

Keep the following identities distinct:

| Identity | Lifetime and purpose |
| --- | --- |
| Keycloak issuer/subject | Exact human owner; mapped by the manager to its assignment |
| Manager instance ID | Stable logical Hermes identity across updates, Pod replacement and retained-state re-add |
| Namespace/Deployment UID | Exact Kubernetes resource incarnation owned by that instance |
| Hermes Pod UID | Exact Pod incarnation, linked through its ReplicaSet to the owned Deployment; changes when the Pod is replaced |
| Container runtime ID | An individual runtime container incarnation; optional diagnostic metadata, not an authentication credential or stable owner identity |

The controller must record and verify the Kubernetes ownership chain and Pod
UID before authorizing native start. Knowing which resources it created lets
the manager correlate the authenticated logical instance with its workload, but
does not authenticate a request by itself. Do not infer authorization from a
source IP, hostname or a self-reported container/Pod ID. The current per-instance
credential authenticates the logical instance, not a particular live process;
proof tied to a specific Pod/container would require a separate authenticated
workload-identity design. No Kubernetes API token needs to be mounted in Hermes
to support the per-instance protocol.

This is the target image/plugin contract. The upstream-based Dockerfile,
bundled plugin, instance credential authentication and recorded Pod ownership
already exist. The [terminal backend guide](terminal-backends.md) describes the
implemented profile selection, v2 provisioning, pinned SSH readiness and cleanup;
the [implementation status](implementation.md) lists remaining integration gates.

## Per-Hermes terminal images and mounts

Terminal backend types are persistent database configuration items. Each has a
stable unique type ID and immutable template versions, independently of the
ephemeral Kubernetes Pod/Deployment UID used by a running worker. The current
model uses a catalog ID such as `python-tools` together with its version; a UUID
is not required for uniqueness. Administration APIs write these PostgreSQL
records rather than maintaining a separate configuration source.

The effective backend template combines its image/resources/SSH definition with
the instance's permitted mount selections. A reusable backend type may declare
mount requirements; concrete PVC bindings remain owner-specific. Each Hermes
assignment explicitly allows type/version references, and the authenticated
plugin requests a type ID rather than submitting an image or Kubernetes spec.
The manager resolves that selection, renders the approved Kubernetes workload
and owns its commissioning/lifecycle. Use a typed template so resource ownership,
credentials, security settings and generation fencing remain manager-controlled.

The Rust manager owns assignment policy, commissions Hermes and creates terminal
worker Pods through its execution module. Creating an assignment does not require
starting an idle terminal Pod or building a new image for that user.

Keep two independent image choices:

| Image | Source and behavior |
| --- | --- |
| Hermes application | Approved instance-template version, normally the platform release's Hermes image; includes native dashboard and compatible adapters |
| Terminal worker | Approved terminal-runtime version selected for this assignment; exact digest may differ between owners or approved runtimes of one owner |

The shipped `agent-runtime` is the default terminal image. Custom images must
pass the managed SSH worker contract: non-root approved UID/GID, SSH on port
2222, user `worker`, selected authorized-key mount, shell/file-transfer support,
approved entrypoint and required native tool helpers. Image pull and compatibility
failures must be reported; never substitute the default image silently. Private
registries use explicit namespace-local pull-Secret references. The new Rust
Kubernetes execution driver must apply those references to worker Pods.

The manager's PostgreSQL records are the desired policy source. Each assignment
selects a default runtime and an explicit set of allowed immutable runtime
versions, with owner-specific mount bindings. Administrators may register exact
custom images through the versioned API/UI. Neither login nor a native Hermes
configuration edit can authorize a new image, mount or runtime version.

An illustrative fragment of an assignment's execution policy is:

```yaml
execution:
  defaultRuntimeId: python-tools
  allowedRuntimes:
    - runtimeId: python-tools
      version: 1
      mounts:
        - storageBindingId: owner-network-home
          mountPath: /mnt/home-network
          readOnly: true
```

This describes a proposed manager API field, not a current Helm value or native
Hermes field. The referenced runtime version contains the exact image digest;
the storage binding resolves only to that assignment owner's approved claim.
Different assignments may select different image versions, mounts and access.

### Hermes selection and manager authority

The native dashboard exposes profile-scoped `terminal.runtime_id`; the plugin
forwards that backend ID as `runtime_id` in lease and session-close requests.
Empty uses the assignment's default. Hermes owners can select a requested type,
while the manager enforces its effective allowlist and resolves the exact
image/mount policy. Lease requests carry no image, template, mount or caller
scope fields. The [terminal backend guide](terminal-backends.md) describes the
implementation.

The manager must supply the verified SSH endpoint and server public key only
after authenticated worker readiness. The plugin uses its mounted per-instance
SSH client key and pins that returned server identity; no private key belongs in
the API response or profile/dashboard configuration. A denied backend must
produce an error without local execution or fallback to another image. Existing
leases stay bound to their granted backend/generation when profile settings
change; newly acquired environments use the new selection.

The authenticated execution API resolves `runtime_id` within the exact client's
instance policy, using the server-selected runtime version. A
runtime name belonging to another owner is denied. Include runtime identity and
effective policy generation in environment/cache identities; closing a session
cannot retire another runtime's environment. Multiple Hermes profiles of one
owner remain distinct from cross-owner authorization.

The shared controller must render terminal worker specs and the exact admission
policy from one PostgreSQL effective policy generation. Requests that omit
`runtime_id` use the assignment's default. There is no static manager YAML or
per-owner database as a second source of truth, and no agent-provided Pod spec.

Image/mount changes create an audited desired generation and an operation.
Validate image compatibility, owner binding, claim/Secret identities and
resources first. Fence new acquisitions and drain affected leases before
activating matching database policy generation and admission rules. Keep
other owners running. Report desired/applied generation separately; a failed
activation must not run a new image with old mounts or silently continue an old
policy as if the requested change succeeded. Never modify a live worker's image
or mounts in place, or delete its owner's retained data to apply a policy.

### Owner storage and network folders

Mounts specify owner-bound storage ID, absolute terminal path, optional bounded
subpath and explicit read-only/read-write access. Validate overlapping/reserved
paths, traversal, claim/Secret identity, namespace and storage permissions.
An authorized mount may expose only a PVC subdirectory: for example, PVC
`owner-data`, relative `subPath: projects/example`, mounted at `/workspace`.
Render Kubernetes `volumeMounts[].subPath`; the container receives that selected
directory at the mount point. Reject absolute subpaths and `.`/`..` traversal.
The exact subpath/access selection must be permitted by the storage binding;
the agent cannot expand it to the PVC root. The PVC must still be available in
the worker's namespace and satisfy its storage/access/permission requirements.
Custom mounts must not replace SSH authorization, internal credentials or
another owner's home. Bind mounts to terminal Pods only unless a separate
approved Hermes application mount is configured. Mount access never implies
permission to browse that folder through the manager administration UI.

A user's network home may use an existing NFS-backed claim or a supported SMB
CSI-backed claim. The deployment/storage adapter provisions or registers the
owner-specific PVC in the instance namespace; the Rust controller mounts that
approved PVC in the terminal Pod. Do not add raw host paths or privileged
in-container mount commands.
See [Kubernetes persistent volumes](https://kubernetes.io/docs/concepts/storage/persistent-volumes/)
and the [SMB CSI driver](https://github.com/kubernetes-csi/csi-driver-smb).

The deployment owns the storage driver, private network route, server/export or
share setup, owner ACLs, UID/GID mapping and credential Secret references.
Verify reachability from the node/CSI mount path as well as any needed Pod path;
a browser or Hermes connection to the home network is insufficient. This does
not grant the worker broad LAN access or the manager cluster-wide storage rights.
Missing required mounts block that terminal runtime with a clear condition;
never substitute an empty local directory. Preserve dashboard/admin access for
repair when its own required storage is healthy.

Register external-storage ownership, retention and backup policy explicitly.
External network-home contents are not automatically included in platform
backups, copied into images or deleted on assignment removal. Recovery inventories
must include bindings and policy versions, while payload backup follows the
selected policy. Restoration validates/rebinds namespace-local claims before
admitting terminal work and reports unavailable external storage without
commissioning replacement data.

## Recovery and installation contract

Require explicit fresh, restore or existing mode. Fresh mode refuses an
unexplained existing registry or owned instance data. Restore mode fences routing
and commissioning until a coordinated recovery receipt has been verified.
Existing mode requires matching installation identity and supported schema.

The versioned inventory must cover registry/assignments, owner keys, instance IDs,
namespaces and resource UIDs, selected PVC datasets and Secrets, native settings
and authentication state, execution environments/leases, template versions and
image digests, terminal-runtime versions, effective policies and external storage
bindings/backup disposition. Inventory generation must fence both administrator
changes and reconciliation; the coordinator must stop all writers before taking
its payload. Fence execution acquisitions, drain terminal work and invalidate
pre-restore leases/endpoints before resuming restored workloads.

Encrypted full-payload readback and a matching inventory/epoch receipt are
mandatory before final teardown. Failed/incomplete snapshots do not become the
latest completed recovery point. Restore must bind new Kubernetes UIDs explicitly
while preserving stable OIDC ownership and instance IDs. Missing registry data
alongside retained homes must block automatic provisioning.

The platform provides lifecycle/inventory contracts and generic validation.
External storage, encryption keys, transport and deployment-specific backup Jobs
remain deployment-owned. A deployment must not run personal instances while its
checkpoint integration backs up only a static dataset list.

The chart's `hermesManager` section deploys the API/dashboard, controller and
router-auth roles with existing database and Secret references, installation
identity and per-listener ingress peers. Routing roles and a recovery gate are
still to be added. Do not put per-user assignments or inline model credentials
into Helm values; assignments live in PostgreSQL. Administrators are authorized
by PostgreSQL grants on Keycloak identities, not a second user-password database.

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
| Repository-built Hermes image | Pinned upstream base plus reviewed platform modifications and bundled repository-owned plugin; no startup plugin download, personal seeds or embedded credentials |
| Plugin requests a terminal on demand | Authenticated acquire causes only that instance's approved worker to be created/reused; verified SSH readiness precedes a usable lease, and renewal/release/session close preserve owner scope |
| Plugin caller identity and forged container metadata | Distinct instance credentials resolve to distinct assignments; a forged user/instance/Pod/container ID or source address cannot select another owner or substitute for authentication |
| Hermes Pod replacement | New Pod UID remains bound to the same logical instance and owner; credentials/state are not reassigned, and an old Pod/container identifier cannot authorize a new start |
| Two owners with distinct terminal images and mounts | Each terminal Pod uses its own exact image digest and authorized claims/access; changing one assignment leaves the other unchanged |
| Same image, different owner storage | Shared image layers do not share homes, network folders, credentials or execution scope |
| Native Hermes runtime selection | An approved `runtime_id` selects its server-defined image and policy; another owner's ID, raw image/mount fields and unsupported versions are rejected |
| Custom terminal image and private registry | Compatible non-root SSH image runs with scoped pull credentials; image/entrypoint incompatibility or pull failure is reported without fallback |
| Terminal policy change during active leases | Only affected work drains; matching database policy/admission activate together and desired/effective status remains accurate |
| Shared manager restart and concurrent execution requests | PostgreSQL recovers environments, requests and leases; no duplicate Pod/lease grant, foreign owner access, expired-lease revival or per-owner manager service |
| Kill manager before/after each commit and Kubernetes call | Resume from PostgreSQL and observed resources; one target generation, preserved owner data and accurate operation status, including create-success-before-UID-save |
| API or lease response lost after commit | Retry the same idempotency key returns the recorded operation/live lease; changed payload conflicts and expired leases are not recreated |
| Stale worker, delayed Kubernetes response or watch gap | Fences/preconditions reject stale progress, uncertain effects are reconciled and periodic resync finds missed work without destructive replay |
| Restart during initialization, policy activation or deletion | Resume the recorded step and credentials; never reseed live state, mix policy generations or report deletion complete before verified absence |
| Enabled Hermes with no browser traffic | One desired gateway replica remains running and a fixture cron job fires; it can acquire an on-demand terminal worker without a user request |
| Idle terminal cleanup | Terminal worker retirement leaves Hermes, its native cron scheduler and retained state running |
| Assignment revoke during terminal acquisition | No new ready lease after revocation; pending terminal generations are fenced and existing work drains within the configured bound |
| Execution client tries another API boundary | Cannot call administration, route resolution or checkpoint methods, or nominate another instance/namespace |
| Owner NFS/SMB folder and unavailable required share | Selected path/access is enforced; no foreign share, host path or empty-directory fallback; external data is retained on revoke/removal |
| UI import/dependency boundary | Presentation cannot import database, directory or Kubernetes adapters |
| Recovery with registry and both users' data | Stable owners/instance IDs restored, new UID bindings verified, stale sessions invalidated |
| Missing registry, partial snapshot or wrong installation receipt | Provisioning/teardown blocked; no automatic adoption or replacement |

Run isolated contract/domain tests, real PostgreSQL concurrency/migration tests
and disposable local Kubernetes/OIDC/browser tests as appropriate. Generic
acceptance uses fixture identities and model stubs; it must not require a paid
GPU, private model account, public invitation or destructive live stack. Record
actual results and untested cases before claiming these requirements are met.
