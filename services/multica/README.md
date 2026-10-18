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

```sh
bin/build multica-backend
bin/up
```

Build/test stays local. The lifecycle records/imports a content-specific image
reference and the platform chart selects it; no platform registry publication
is required. Native login files and agent configuration are separate state.

After deployment, an administrator can run
`python3 tests/integration/check_multica_token_boundary.py`. It creates one closed,
unassigned issue and an already completed queue row, then authenticates a
short-lived native task token against the real backend. The probe checks issue
access, credential-administration denial, forged actor headers, and continued
owner/profile PAT access. It removes its issue/task/token in `finally` and emits
only HTTP statuses and cleanup counts. It never dispatches a model or renews
an existing credential. Real model acceptance separately verifies the injected
MAT from a normal Multica task.
