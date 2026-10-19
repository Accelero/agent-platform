# Personal Hermes implementation evidence

Branch: `feat/personal-hermes-manager`. Release 0.6.1 remains the current
published application. This file tracks evidence against the full
[plan](plan.md), not a replacement scope or a claim of release readiness.

## Implemented foundation

- Rust workspace with domain, application and PostgreSQL adapters, pinned
  toolchain and locked dependencies.
- Exact case-sensitive issuer/subject keys, immutable database ownership,
  approved versioned instance-template references and stable private namespace
  names. Display/email matching cannot grant ownership.
- Explicit administrator audience/client-role checks on application commands.
  Transport authentication, browser sessions and CSRF still need implementation.
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

## Evidence and remaining gates

| Area | Executable evidence | Still required |
| --- | --- | --- |
| Identity/domain | Rust domain/application tests; signed-JWT native provider test in pinned Hermes | Real router/admin OIDC adapter, full native session/middleware coverage, public-subject compatibility |
| Assignment persistence and resolver | 20 PostgreSQL migration/concurrency/recovery/routing tests | HTTP API contracts/handlers and cryptographic authentication |
| Crash journal | PostgreSQL intent, lease/fence, unknown-result and delete-absence tests | Owned-resource registry, real Kubernetes verification, forced-process-death tests at every external boundary |
| Proxy plugin | Go tests with race detector | Full native HTTP/upload/browser/cookie behavior and configured provider revocation |
| Traefik | `tests/personal_hermes/router_compatibility.py`, pinned official image | End-to-end Rust/Keycloak/native Hermes acceptance |
| Existing release | Python checks, 88 pytest tests, Helm lint, release privacy preflight | Repeat after chart/runtime changes |
| Runtime policy/execution | Domain mount-path validation | Catalog/storage/effective-policy database model, execution API, environment/lease transactions, SSH driver and adapter extension |
| Commissioning | Durable outbox and stable instance reservation | Controller process, bootstrap RBAC/admission, ownership intents/UIDs, templates, create-once secrets, home initialization, readiness and retained suspension |
| Administration UI | Application ports separate presentation from persistence | Administration/owner/checkpoint OpenAPI, API/UI role, OIDC/CSRF, generated client and React/Vite build |
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

All P01–P18 remain subject to integrated verification. The source foundation does
not yet commission a Kubernetes instance or expose a production manager/router.
No phase is complete merely because its unit tests pass. The source proofs must
be combined with real database, Kubernetes, provider, native application and
browser evidence before the feature is enabled or released.
