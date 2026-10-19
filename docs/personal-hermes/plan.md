# Personal Hermes implementation plan

Status: design for a future release. The services described here are not
implemented in release 0.6.1. This change adds documentation only.

Read the [specification](specification.md) for required behavior and the
[architecture](architecture.md) for component boundaries and deployment decisions.

## Product decisions

- Keycloak authenticates people and supplies stable OIDC identities.
- An explicit assignment in the manager's PostgreSQL database decides whether a
  person gets a personal Hermes instance. A Keycloak user or group membership
  alone does not create that assignment.
- Build the assignment administration interface into `hermes-manager` first.
  Its browser UI uses a versioned administration API. Domain logic, persistence,
  identity-directory access and Kubernetes reconciliation stay behind separate
  interfaces. A future admin-panel service must need only that API.
- Enabling an assignment commissions its instance proactively. First login can
  report or retry existing work, but cannot grant itself an instance.
- `hermes-router` serves one common Hermes hostname and selects the backend from
  the authenticated issuer/subject and current assignment. No user-selected
  backend, email-based routing or fallback to a shared owner's instance.
- Each owner gets a namespace, persistent state, native authentication keys and
  credentials. Reuse the existing execution `runtime-manager` inside that
  namespace with an exact, owner-specific policy.
- Keep Multica accounts, board ownership and enrollment independent. Do not
  enroll new users under a shared board owner or copy that owner's model login.

## Delivery sequence

Every phase below is proposed work. Complete its exit criteria before taking the
next phase into a release. Keep the existing singleton installation available
until the explicit adoption phase succeeds.

### 0. Prove identity and isolation assumptions

- [ ] Record the supported native Hermes revision and public APIs used for
  authentication, sessions, logout, uploads, HTTP mutations and WebSockets.
- [ ] Prove the router OIDC flow and native Hermes flow can share a public
  hostname with distinct callback paths, using Keycloak SSO between the flows.
- [ ] Require the same public subject identifier across router and native
  clients. Reject unsupported pairwise-subject configurations instead of
  correlating users by email.
- [ ] Add or adapt native authentication to pin an instance to its expected
  issuer/subject on login, session verification and token refresh. A global
  Hermes-access role does not establish instance ownership.
- [ ] Prove a standalone personal instance starts without another person's
  Multica enrollment, provider login, prompts or files.
- [ ] Prototype namespace creation and constrained RoleBinding bootstrap using
  the intended manager ServiceAccount. Test the negative admission cases before
  granting production Kubernetes permissions.

Exit: two fixture identities can authenticate to their own native instances;
foreign identities, callbacks and native cookies are rejected. The namespace
bootstrap cannot grant the controller access to an unrelated namespace.

### 1. Define the manager contracts and durable model

- [ ] Add typed domain/application packages and adapter interfaces described in
  the architecture. Keep HTTP/UI, PostgreSQL, Keycloak and Kubernetes code out of
  the domain package.
- [ ] Publish an OpenAPI contract for the administration, router-resolution,
  owner-status and checkpoint APIs before implementing the UI.
- [ ] Implement versioned SQL migrations, unique identity/assignment constraints,
  desired and observed generations, operation leases, idempotency and audit data.
- [ ] Support an explicit fresh/restore/existing installation gate. Missing
  registry state alongside existing instance data must require recovery.
- [ ] Define compatibility checks for instance-template versions and database
  migrations. An unsupported schema must stop reconciliation.

Exit: application tests exercise assignment lifecycle and concurrent commands
through ports; SQL integration tests prove uniqueness and recovery of interrupted
operations. No Kubernetes access is needed for these tests.

### 2. Build the integrated management interface

- [ ] Implement manager administration login with a dedicated OIDC client and
  configured platform-administrator role. A personal Hermes assignment grants
  no platform-administration permission.
- [ ] Implement the versioned administration API, authorization, CSRF protection,
  optimistic concurrency and actor-attributed audit events.
- [ ] Add the built-in UI for identity lookup, assignment creation, enable,
  suspend, revoke, template selection, instance status and safe retry.
- [ ] Support exact issuer/subject entry and an optional read-only Keycloak
  directory adapter. Show email/display name as descriptive fields only.
- [ ] Keep model secrets, private files and chat histories out of administrator
  responses. Do not add an impersonation or arbitrary Kubernetes editor feature.
- [ ] Add an API-only mode with the built-in UI disabled. Prove a small external
  client can perform the same workflow without database or Kubernetes access.

Exit: the UI manages fixture assignments only through the administration API.
UI/API contract tests and an API-only acceptance test demonstrate that the UI
can later move to a separate service without changing the core use cases.

### 3. Implement commissioning and reconciliation

- [ ] Run API/UI and controller roles from one `hermes-manager` image in separate
  Deployments. Give only the controller a Kubernetes credential.
- [ ] Create deterministic, installation-owned namespaces, storage, Services,
  Secrets, ServiceAccounts, policies, Hermes and execution-manager workloads.
- [ ] Bootstrap namespace access through the restricted RBAC/admission mechanism
  in the architecture; preserve resource UID and installation checks.
- [ ] Generate exact native configuration from reviewed platform templates.
  Configure one distinct execution-manager client scope per owner. No wildcard
  profile, volume, Secret, image or service-account authorization.
- [ ] Initialize private homes only when missing. Persist native settings,
  profiles, memory, uploads and user-created agents across reconciliation.
- [ ] Implement capacity limits, bounded work queues, readiness conditions,
  retry/backoff and crash recovery after each provisioning step.
- [ ] Deny routing before suspension/decommissioning work, drain bounded active
  work and retain storage. Make permanent purge a separate explicit operation.

Exit: concurrent enable/login/retry operations produce one instance per identity.
Controller restart, lost responses, stale workers and conflicting object UIDs
cannot duplicate instances or adopt foreign resources. Two users' execution Pods
cannot select each other's homes or credentials.

### 4. Implement authenticated routing and native sessions

- [ ] Add the `hermes-router` image, OIDC login and server-side session store.
- [ ] Resolve each request through the manager's internal routing API. Keep the
  router free of Kubernetes credentials and assignment database permissions.
- [ ] Implement the separate platform/native callback routes, exact native owner
  binding, isolated cookies and coordinated logout.
- [ ] Forward ordinary dashboard HTTP, streaming responses, uploads and
  authenticated WebSocket upgrades. Preserve native single-use ticket checks.
- [ ] Enforce disabled-assignment and expired-identity behavior for existing
  WebSockets as well as new HTTP requests. Never fail open on manager failure.
- [ ] Test identity switching, back/forward navigation, overlapping login flows,
  stale cookies, forged headers, invalid audiences and attempted backend SSRF.

Exit: two fixture users use the same HTTPS hostname and see independent Hermes
settings and sessions. Full native HTTP and WebSocket tests run through the
router, not directly against a backend.

### 5. Make dynamic instances recoverable

- [ ] Implement the versioned inventory and checkpoint interfaces without
  embedding a deployment's S3 credentials or backup scripts in this repository.
- [ ] Fence assignment changes and reconciliation during checkpoint/restore;
  drain execution, stop writers and export registry plus complete instance data.
- [ ] Require encrypted full-payload readback before a completion receipt permits
  teardown. A failed checkpoint keeps the deletion gate closed.
- [ ] Restore instance IDs, ownership, assignments and data together. Validate
  new Kubernetes UID bindings under an explicit restore epoch.
- [ ] Prove that loss of the registry never silently creates replacements for
  existing homes, and that replayed/mismatched completion receipts are rejected.
- [ ] Cover partial restore, abandoned work leases, retained-volume reattachment
  and rotation of invalidated sessions without copying another owner's secrets.

Exit: a disposable-cluster recovery test restores both fixture users and their
distinct data, with ownership and routing unchanged. A checkpoint failure blocks
the planned teardown rather than bypassing retention safeguards.

### 6. Package the opt-in chart feature and qualify a release

- [ ] Add the proposed personal-instance values and strict schema validation.
  Leave the feature disabled by default and preserve legacy singleton values.
- [ ] Package both new images, templates, schemas and policies in the same
  immutable chart release as the existing runtime images.
- [ ] Render manager native configuration and admission rules in the platform;
  infrastructure consumers supply values and Secret references only.
- [ ] Add chart tests for incompatible singleton/personal modes, missing OIDC or
  recovery prerequisites, unsupported Kubernetes policy support and unsafe RBAC.
- [ ] Complete the acceptance matrix in the specification, repository checks,
  release privacy review and anonymous chart/image pull verification.
- [ ] Document operator installation, adding/disabling people, model onboarding,
  incidents, backup/restore and the separate-panel extraction procedure.

Exit: one coherent release provides the manager, built-in UI, router and tested
per-instance policy. Documentation alone must not bump versions, build a new
runtime release or start a cloud stack.

### 7. Adopt an existing deployment explicitly

- [ ] Extend the consumer's static storage/Secret backup lists to the versioned
  instance inventory before enabling personal mode.
- [ ] Import the designated owner's existing home using an explicit verified
  issuer/subject assignment and a checked source-to-target storage manifest.
- [ ] Preserve the original board enrollment only when it belongs to that same
  owner. Other people start without it.
- [ ] Switch the common ingress to the router after adoption checks pass; retain
  a documented rollback boundary until the first coordinated checkpoint.
- [ ] Review the consumer's resource identities, preview and authorized rollout.
  Never enable automatic replacement to evade an immutable-storage migration.

Exit: the original owner's state survives, new people receive isolated
instances, and the consumer can stop and restore the installation safely.

## Deferred work

A separately deployed administration panel is a later packaging change against
the same API. Team/shared Hermes instances, automatic Multica enrollment,
cross-owner impersonation, OAuth token exchange/on-behalf-of-user execution,
arbitrary Kubernetes templates and a shared dynamic execution-manager catalog
are outside this first implementation. The current per-instance execution
manager avoids weakening the existing static execution authorization boundary.
