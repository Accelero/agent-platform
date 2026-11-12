# Compile a surgical native auth fix against the exact public source revision
# used by MULTICA_BACKEND_IMAGE. The vendor checkout is never modified.
ARG MULTICA_BACKEND_IMAGE=ghcr.io/multica-ai/multica-backend@sha256:60dc856bc44c01a3c73e8153e211be6c22cc6cbb66dde8b20e01de5b6e75e41f
FROM golang:1.26-alpine AS source
RUN apk add --no-cache patch
WORKDIR /src
COPY --from=multica-source /server/go.mod /server/go.sum ./server/
RUN --mount=type=cache,target=/go/pkg/mod cd server && go mod download
COPY --from=multica-source /server/ ./server/
COPY auth-boundary.patch /tmp/auth-boundary.patch
RUN patch --batch --forward --fuzz=0 -p1 < /tmp/auth-boundary.patch
COPY bootstrap/ ./server/cmd/platform-bootstrap/

# The `test` target exports the Go results; the multica-unit pytest suite reads
# them. The focused packages have no upstream TestMain that silently skips
# without a database.
FROM source AS go-test
COPY auth-boundary_test.go ./server/platformauth/auth_boundary_test.go
COPY maintenance_linux_test.go ./server/platformauth/maintenance_linux_test.go
RUN --mount=type=cache,target=/go/pkg/mod --mount=type=cache,target=/root/.cache/go-build \
    mkdir /results && cd server \
 && CGO_ENABLED=0 go test -json -count=1 ./platformauth ./cmd/platform-bootstrap \
      > /results/go-test.json; echo $? > /results/status

FROM scratch AS test
COPY --from=go-test /results/ /

FROM source AS builder
ARG VERSION=v0.6.0
ARG COMMIT=ea94c7cd5bbce9c8e1f28c5fa049c47ee7651d02
RUN --mount=type=cache,target=/go/pkg/mod --mount=type=cache,target=/root/.cache/go-build \
    cd server && CGO_ENABLED=0 go build -o /out/multica-bootstrap ./cmd/platform-bootstrap \
 && CGO_ENABLED=0 go build -ldflags "-s -w -X main.version=${VERSION} -X main.commit=${COMMIT}" \
       -o /out/server ./cmd/server

FROM ${MULTICA_BACKEND_IMAGE}
COPY --from=builder /out/server /app/server
COPY --from=builder /out/multica-bootstrap /usr/local/bin/multica-bootstrap
LABEL org.hermes.multica-auth-boundary="human-credential-administration-v1"
