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

The local `multica-backend` Bake target tests the actual Auth and handlers, then
compiles the patched server. Its focused Go package avoids upstream test runners
that exit successfully without running tests when PostgreSQL is absent. Only
the SQL boundary is a test fixture. The final image replaces `/app/server` in
the matching pinned runtime; migrations, entrypoint, CLI and notices remain.
There is no runtime proxy or credential helper.

The independent release workflow builds and tests this target with the other
runtime images, then pins its digest in the application chart. Native login files and agent configuration are separate state.

The build runs the focused auth and maintenance Go tests without live service
credentials. Application login, email delivery and model dispatch are separate
deployment acceptance checks.
