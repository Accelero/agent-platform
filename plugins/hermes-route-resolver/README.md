# Personal Hermes routing middleware

The Go/Yaegi plugin is the narrow routing boundary between authenticated
ForwardAuth and Traefik child-route selection. It queries the manager for every
HTTP request and WebSocket opening handshake. It sets only the registered
instance ID and route generation; it never creates or dials backend URLs.

Both internal clients require mTLS with referenced CA/client certificates. User
evidence must come from the preceding Rust authentication adapter, and the Rust
manager must independently verify its signature, issuer, audience, authorized
client, age and exact subject. The plugin has no SQL, Kubernetes or OIDC SDK.

Existing WebSockets are tracked at the hijacked downstream connection. Every
five seconds the plugin reauthorizes the server-side session through the auth
adapter, obtains current evidence and re-resolves the assignment. A changed
identity, instance, generation or authorization version closes the connection.
Each HTTP call is bounded to two seconds and each recheck cycle to four seconds,
within the specification's ten-second revocation bound. There is no positive
assignment cache or automatic replay of native mutations.

The production auth adapter and manager endpoints are still being implemented.
This plugin's fixtures are not substitutes for the OIDC/native session tests.
Reserved cookies, native owner pinning, browser callbacks, provider revocation,
admission and chart packaging remain qualification gates.

```bash
cd plugins/hermes-route-resolver
go vet ./...
go test -race -count=1 ./...
```

From the repository root, Docker, OpenSSL and Python 3.14 run the actual Traefik
compatibility test:

```bash
uv run python tests/personal_hermes/router_compatibility.py
```

The test uses pinned Traefik 3.7.15, ephemeral mTLS certificates, two fixture
backends and a fixture authentication/resolution service. It loads the plugin
through Yaegi, exercises the ordered parent/child routers and observes actual
WebSocket closure. The container runs without root, capabilities, a writable
root filesystem or a Kubernetes credential. Local host networking connects
only to ephemeral listeners bound to loopback. All fixture containers and
temporary private keys are removed after the test.
