# Shared manager, dashboard and router-auth release image.
FROM node@sha256:86f07bc9c5dce4578cf37e5a418b7bfc7f817cda25cde66e2b66e95ed86c4567 AS ui
WORKDIR /src/services/hermes-manager/ui
COPY services/hermes-manager/ui/package.json services/hermes-manager/ui/package-lock.json services/hermes-manager/ui/.npmrc ./
RUN npm ci --ignore-scripts --fetch-timeout=60000 --fetch-retries=2
COPY contracts/hermes-manager/openapi.yaml /src/contracts/hermes-manager/openapi.yaml
COPY contracts/hermes-manager/router.openapi.yaml /src/contracts/hermes-manager/router.openapi.yaml
COPY contracts/hermes-manager/schemas/ /src/contracts/hermes-manager/schemas/
COPY services/hermes-manager/ui/ ./
RUN npm run generate && npm run check:schemas && npm test && npm run build

FROM rust@sha256:a24e292b4af7bdee351a31d3a6379972a5652f0ea22a1b6d6883324ac4f5097c AS rust
WORKDIR /src/services/hermes-manager
COPY services/hermes-manager/Cargo.toml services/hermes-manager/Cargo.lock services/hermes-manager/rust-toolchain.toml ./
COPY services/hermes-manager/crates/ ./crates/
COPY contracts/hermes-manager/schemas/ /src/contracts/hermes-manager/schemas/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/services/hermes-manager/target \
    cargo build --workspace --bins --release --locked \
 && mkdir /out \
 && cp target/release/hermes-manager target/release/hermes-router-auth /out/

FROM debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
ARG VERSION=development
ARG REVISION=unknown
LABEL org.opencontainers.image.source="https://github.com/Accelero/agent-platform" \
      org.opencontainers.image.version="$VERSION" \
      org.opencontainers.image.revision="$REVISION"
RUN apt-get update \
 && apt-get install --yes --no-install-recommends ca-certificates openssh-client \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 manager \
 && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent manager
COPY --from=rust /out/ /usr/local/bin/
COPY --from=ui /src/services/hermes-manager/ui/dist/ /usr/share/hermes-manager/ui/
COPY --from=ui /src/services/hermes-manager/ui/dist/THIRD_PARTY_NOTICES.txt /usr/share/doc/hermes-manager/THIRD_PARTY_NOTICES.txt
COPY --from=ui /src/services/hermes-manager/ui/node_modules/react/LICENSE /usr/share/doc/hermes-manager/react.LICENSE
COPY --from=ui /src/services/hermes-manager/ui/node_modules/react-dom/LICENSE /usr/share/doc/hermes-manager/react-dom.LICENSE
USER 10001:10001
WORKDIR /tmp
EXPOSE 8443 8444 8445
ENTRYPOINT ["hermes-manager"]
CMD ["api", "--config", "/run/manager/config.json"]
