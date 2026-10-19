# Personal Hermes implementation evidence

Branch: `feat/personal-hermes-commissioning`. Release 0.6.1 remains the current
published application. This file tracks evidence against the full
[plan](plan.md), not a replacement scope or a claim of release readiness.

## Implemented foundation

- Rust workspace with domain, application and PostgreSQL adapters, pinned
  toolchain and locked dependencies.
- Exact case-sensitive issuer/subject keys, immutable database ownership,
  approved versioned instance-template references and stable private namespace
  names. Display/email matching cannot grant ownership.
- Explicit administrator audience/client-role checks on application commands,
  with a TLS API-only HTTP transport and verified Keycloak access-token adapter.
  Optional browser OIDC login, encrypted session persistence and CSRF use the
  same versioned API; presentation serving remains outstanding.
- Transactional create/access/retry commands: optimistic versions, stable
  instance reservation, actor-scoped body-checked idempotency, immutable audit
  and proactive durable commissioning work. Revoke retains records and retry
  cannot enable an assignment.
- Versioned logged PostgreSQL registry, bounded database calls, explicit
  installation initialization and existing-mode schema/identity/epoch checks.
- Leased/fenced operation claims and immutable action-step journal. Lost-response
  replay, expired-claim recovery, uncertain initialization blocking and
  exact-UID deletion-result validation have database tests.
- Go middleware with fixed mTLS endpoints, bounded manager lookups, strict
  response validation, no positive cache, instance/generation selection and
  periodic authorization of open WebSockets.
- Rust resolver application policy requires both the configured verified mTLS
  caller and fresh, exact, audience/client-bound user evidence. Its PostgreSQL
  reader checks current access, instance readiness/generations, template port
  and namespace UID bindings on every request, without commissioning side effects.
- Native OIDC owner hook applied to the exact pinned upstream source with a
  build-time SHA-256 guard. The hook checks verified issuer/subject on login,
  session verification and refresh; incomplete pin configuration fails closed.
  Both values absent preserve the explicit legacy singleton behavior.
- Compiled `hermes-manager api/migrate/check` process roles, reference-based
  secret/configuration inputs, separate administrator TLS and resolver mTLS
  listeners, bounded request/connection/header/body handling and redacted errors.
- Administration OpenAPI contract and HTTP workflow for identities, templates,
  assignment create/access/retry, instance/operation metadata and immutable audit.
  An omitted template version is pinned once without changing replay semantics.
- Cryptographic access-token checks pin signature algorithm, issuer, audience,
  authorized client, user-session identity and lifetime; route evidence is also
  age-bounded. JWKS calls are fixed-origin HTTPS, bounded and size-limited with
  no redirects/proxy environment fallback. A service certificate alone cannot
  assert a user or administration permission.
- OIDC code flow through the maintained protocol library with S256 PKCE,
  single-use browser-bound state, nonce/signature/issuer/audience/subject checks
  and encrypted server-side tokens. Separate administrator/router session
  schemas and SQL roles, absolute expiry, generation-fenced login/logout and
  installation epoch mirrors. Refresh claims commit before provider calls;
  uncertain refresh outcomes require a fresh login. Bounded credential cleanup.
- Compiled `hermes-router-auth` process with separate public and internal mTLS
  listeners. Distinct service identities protect ForwardAuth and established
  connection session checks. Public session metadata contains no provider token;
  authentication cannot create assignments or access assignment SQL.
- Owned-resource reservations and atomic action journals, authenticated creation
  provenance, first UID/namespace binding and immutable ownership/tombstones.
  The application resource reconciler uses a bounded kube-rs adapter, checks the
  live cluster/namespace UID, preserves resource versions in write/delete intents
  and verifies absence before completing retirement. Retained data has no
  ordinary delete path.
- Create-once bootstrap vault with authenticated encryption bound to installation,
  instance, operation, epoch, generation and purpose. Concurrent candidates reuse
  the committed winner; missing keys block instead of regenerating credentials.
  Original native/SSH/execution keys and seed survive template changes.
- Typed personal workload renderer with separate retained home/workspace claims,
  default-deny networking, non-root restricted Pods, disabled service-account
  token mounting and one desired gateway/dashboard replica. Selected Secret files
  keep the initialization key out of the running application. This renderer is
  not yet connected to a controller or approved template-definition registry.
- Native home initializer with durable signed intent/completion receipts,
  no-follow home I/O, atomic file creation and missing-only seeding. Rust verifies
  the Python completion receipt for the exact owner and material. Abrupt death
  before/after config creation resumes without reseeding; completed initialization
  preserves live settings, profiles, agents, memory and enrollment.
- Direct non-root native supervisor for the gateway and dashboard, using a mounted
  owner policy that wins over mutable native environment settings. Optional private
  OIDC trust augments public roots for independent model-provider onboarding.
  Runtime Python tooling targets the pinned image's Python 3.13 separately from
  the repository's Python 3.14 operational code.

## Evidence and remaining gates

| Area | Executable evidence | Still required |
| --- | --- | --- |
| Identity/domain | Rust domain/application tests; signed-JWT native provider test; real HTTPS OIDC/access-token/session checks | Full native session/middleware coverage, configured Keycloak/public-subject compatibility and external revocation bound |
| Assignment persistence and resolver | 22 registry tests, ten session/crypto tests, seven token/API HTTPS tests and eight browser/router HTTPS tests, API-only workflow and OpenAPI validation | Remaining runtime/storage/owner/checkpoint endpoints and integrated provider acceptance |
| Crash journal and ownership | Registry tests plus ten resource/proof tests; real Kubernetes create-before-UID-save recovery, current-version update/retry, foreign UID/proof rejection and finalizer/absence tests | Full controller integration and forced-process-death tests at every external boundary |
| Proxy plugin | Go tests with race detector | Full native HTTP/upload/browser/cookie behavior and configured provider revocation |
| Traefik | `tests/personal_hermes/router_compatibility.py`, pinned official image | End-to-end Rust/Keycloak/native Hermes acceptance |
| Existing release | Python checks including OpenAPI validation and native initialization, Helm lint, release privacy preflight | Repeat after chart/runtime changes |
| Runtime policy/execution | Domain mount-path validation | Catalog/storage/effective-policy database model, execution API, environment/lease transactions, SSH driver and adapter extension |
| Commissioning | Durable outbox/resource reservations, authenticated ownership/UID adapter/reconciler; six encrypted-material tests, renderer isolation tests, cross-language receipt verification and 15 initialization tests; built native image startup/restart proof | Controller process, bootstrap RBAC/admission, persisted template definitions, readiness, execution policy and retained suspension integration |
| Administration UI | Versioned administration contract, bearer and browser-session API; presentation-separated application ports | Remaining runtime/storage/owner/checkpoint contracts, generated client and React/Vite build |
| Recovery | Fresh/existing registry gate and maintenance/epoch fencing | Trusted inventory scanner, checkpoint/receipt/restore lifecycle and coordinated full-data recovery tests |
| Distribution | New workspace/plugin checks in CI | Dockerfiles, reviewed release inputs, opt-in chart values/schema/RBAC/networking, operator docs and acceptance qualification |
| Adoption | Existing singleton code/chart untouched | Explicit deployment-specific storage/ownership migration after all platform gates pass |

The real Traefik 3.7.15/Yaegi compatibility test passed on 2026-10-19. Two fixture
owners reached distinct backends through the same hostname/path; forged headers,
an unassigned session and a foreign hostname could not select a backend. Existing
WebSockets closed in 5.00–5.05 seconds after assignment/session revocation,
identity switching or manager failure. Directory-mounted atomic snapshots and
stale generation denial also passed. These fixtures do not prove the remaining
Keycloak or native Hermes authentication requirements.

The native owner test runs the real self-hosted provider in
`nousresearch/hermes-agent@sha256:d4da4a40cd7a28aba983775d9fd31d94cbf153eeb0cb9e844d6d0f612b7c24db`,
upstream revision `749220ef0007f8d87bd1531f1c24b0fe93816385`. Real RSA signatures
are verified; token endpoint responses are fixtures. The expected owner succeeds
through login, session verification and refresh. A different/case-changed subject
with the same email is rejected, changing the email preserves ownership, invalid
signature/issuer is rejected, malformed pin configuration fails, and unpinned
singleton compatibility passes. Run `uv run python tests/personal_hermes/native_owner.py`.

The compiled API/router-auth process test passes migration/startup/TLS boundary
checks, abrupt termination, registry reopening and mismatched installation/epoch rejection.
The seven real HTTPS tests exercise the Rust API and resolver with generated
certificates and signed tokens, including live access revocation, wrong mTLS
identity, token claim/signature failures, input bounds, JWKS outage and a database
lock that must deny within the two-second route-resolution bound. These prove
transport/token behavior; they do not replace a real Keycloak/browser acceptance
run or forced-death tests around Kubernetes/execution actions.

All P01–P18 remain subject to integrated verification. The implementation does
not yet commission a Kubernetes instance and is not feature-complete for release.
No phase is complete merely because its unit tests pass. The source proofs must
be combined with real database, Kubernetes, provider, native application and
browser evidence before the feature is enabled or released.

The explicit Kubernetes test uses a new isolated
`rancher/k3s@sha256:ca7f37d993d82ef0dcdcfecb2e0e2618ea541dbaffc620c8cedebe01a82acd0d`
API server and temporary fixture credentials. It passed on 2026-10-20. It recovers
a Namespace created before its UID was committed, rejects the expired worker,
updates an owned ConfigMap without changing its UID, rejects a copied-proof
replacement and a changed unbound manifest, rejects stale destructive resource
versions, and holds retirement until finalizers are removed and absence is
observed. This is a transport/ownership proof with fixture administrator access,
not admission, non-root Pod, storage, cron or full commissioning acceptance.

The personal native runtime test builds the actual Hermes Dockerfile using only
`release-policy.json` inputs. It runs both gateway and dashboard as UID/GID 1000,
with no capabilities, a read-only root filesystem and no network. Without a model
account, both native processes stay running during HTTP inactivity. The dashboard
advertises only self-hosted OIDC, private APIs return 401 and HTML redirects to
native login. A forced container kill/restart preserves the initialized home and
owner memory, and the application cannot read the init-only key. The optional
private CA is tested alongside retained public roots. Run
`uv run python -m tests.personal_hermes.native_runtime --build`. This offline
fixture proves native startup/authentication gating and process recovery, not a
completed provider login, cron side-effect delivery or Kubernetes commissioning.
