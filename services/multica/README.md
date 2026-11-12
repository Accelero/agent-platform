# Native Multica credential boundary

`auth-boundary.patch` applies at build time to the pinned public
`vendor/multica` source; the checkout remains unchanged. The pin and upstream
runtime image both use revision `ea94c7cd5bbce9c8e1f28c5fa049c47ee7651d02`
(`v0.6.0`). Update source/image pins together and review the patch on an upgrade.

The upstream task-token middleware stamps the owning user ID so agents can act
within their task/workspace. The personal token routes and CLI JWT issuance
previously treated that user ID as sufficient authority. The patch applies the
existing native `RequireHumanActor` guard to `/api/tokens` and
`POST /api/cli-token`, and adds direct-handler checks for list/create/revoke/renew
and JWT issuance. Task and cloud-machine credentials receive 403; normal human
PAT/JWT behavior remains available. The auth middleware's server-set actor
classification also defeats client-supplied actor headers.

The Dockerfile's `test` target runs `go test -json` on the actual Auth and
handlers and on the bootstrap command; the `multica-unit` pytest suite builds it
and reports every Go test separately (see [testing](../../docs/testing.md)). Its
focused Go packages avoid upstream test runners that exit successfully without
running tests when PostgreSQL is absent. Only the SQL boundary is a test fixture.
The image build compiles the patched server without rerunning those tests; the
release workflow publishes only after the `multica-unit` and
`multica-integration` suites pass. The final image replaces `/app/server` in the
matching pinned runtime; migrations, entrypoint, CLI and notices remain.
The application HTTP credential endpoints retain their native authorization.
Native login files and agent configuration are separate state.

The Go tests need no live service credentials. `multica-integration` runs native
bootstrap against the built image and a disposable PostgreSQL. Application
login, email delivery and model dispatch are separate deployment acceptance
checks.

## Offline service bootstrap

`bootstrap/` builds `/usr/local/bin/multica-bootstrap` against the same pinned
native auth, generated database queries and issue-status initializer. The tools
image receives this static binary through its Bake dependency, alongside the
generic Python Job orchestrator. This administrator capability has no HTTP route
and does not change the normal login or human-token authorization boundary.

The command reads bounded JSON on stdin and the database URL from its environment.
It generates distinct native PATs, atomically initializes an empty installation's
user, owner membership, workspace, seven built-in statuses and token hashes, and
records stable IDs in a small platform journal. Raw tokens only pass through the
privileged Job's memory and its runtime Secret. The journal does not add foreign
keys that would prevent ordinary user/workspace deletion. An explicit repair can
reuse the journal's owner or verify the saved owner's native token hash and
workspace membership. It never assigns another user's runtime to the seed email.
