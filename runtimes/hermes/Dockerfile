ARG MULTICA_BACKEND_IMAGE=ghcr.io/multica-ai/multica-backend@sha256:74e7bdddef780b925652f4518f18f54ab790bdfe1ec7803b4dac2019ee2525be
ARG HERMES_IMAGE=nousresearch/hermes-agent@sha256:276d44685c65c0cb2a25f190b63c65c8906f9dc76486fb1c05ae71b5826bda08
ARG UV_IMAGE=ghcr.io/astral-sh/uv:0.13.8@sha256:5cc1805e2426484abca4ad46ea783bab281fdb225fd82d8ce6d66a81254bac85
FROM ${MULTICA_BACKEND_IMAGE} AS multica
FROM ${UV_IMAGE} AS uv
FROM ${HERMES_IMAGE}
USER root
COPY --from=multica /app/multica /usr/local/bin/multica
COPY --from=multica /app/LICENSE /app/NOTICE /usr/local/share/multica/
COPY runtimes/hermes/multica-run /etc/s6-overlay/s6-rc.d/multica/run
COPY runtimes/agent-runtime/multica-checkout-proxy.py /usr/local/lib/multica-checkout-proxy.py
COPY runtimes/hermes/multica-checkout-run /etc/s6-overlay/s6-rc.d/multica-checkout/run
COPY runtimes/hermes/managed-exec /usr/local/bin/managed-exec
RUN mkdir -p /workspace \
 && chmod 755 /etc/s6-overlay/s6-rc.d/multica/run \
 && chmod 755 /usr/local/bin/managed-exec \
 && echo longrun > /etc/s6-overlay/s6-rc.d/multica/type \
 && mkdir -p /etc/s6-overlay/s6-rc.d/multica/dependencies.d \
 && touch /etc/s6-overlay/s6-rc.d/multica/dependencies.d/base \
          /etc/s6-overlay/s6-rc.d/user/contents.d/multica \
 && chmod 755 /etc/s6-overlay/s6-rc.d/multica-checkout/run \
 && echo longrun > /etc/s6-overlay/s6-rc.d/multica-checkout/type \
 && mkdir -p /etc/s6-overlay/s6-rc.d/multica-checkout/dependencies.d \
 && touch /etc/s6-overlay/s6-rc.d/multica-checkout/dependencies.d/base \
          /etc/s6-overlay/s6-rc.d/user/contents.d/multica-checkout

# Generic runtime adapters and optional dependencies travel with the image. The
# pinned uv is mounted for installation only; Hermes keeps its own tool copy.
COPY runtimes/hermes/requirements.lock /tmp/assistant-requirements.lock
RUN --mount=from=uv,source=/uv,target=/usr/local/bin/uv \
    uv --no-config pip install --python /opt/hermes/.venv/bin/python --no-deps -r /tmp/assistant-requirements.lock \
 && HERMES_HOME=/opt/hermes-stack/plugin-home /opt/hermes/.venv/bin/hermes plugins install hindsight --ref f14cc1eb5624b0374cb4d82768eb2e0b9c21ae24 --no-deps --no-enable
# Bundled backend plugins load for every profile, including ones created later.
COPY runtimes/hermes/plugins/managed_execution /opt/hermes/plugins/managed_execution
COPY config/managed-execution /etc/hermes
RUN chmod -R a+rX /opt/hermes/plugins/managed_execution
COPY runtimes/hermes/native_files.py runtimes/hermes/runtime-init.py /usr/local/lib/
COPY runtimes/hermes/assistant-init /etc/cont-init.d/019-hermes-runtime
RUN chmod 755 /etc/cont-init.d/019-hermes-runtime \
 && chmod -R a+rX /opt/hermes-stack

RUN --mount=from=uv,source=/uv,target=/usr/local/bin/uv \
    uv --no-config pip install --python /opt/hermes/.venv/bin/python ddgs==9.16.1
