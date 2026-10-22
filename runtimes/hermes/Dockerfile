ARG MULTICA_BACKEND_IMAGE=ghcr.io/multica-ai/multica-backend@sha256:60dc856bc44c01a3c73e8153e211be6c22cc6cbb66dde8b20e01de5b6e75e41f
ARG HERMES_IMAGE=nousresearch/hermes-agent@sha256:d4da4a40cd7a28aba983775d9fd31d94cbf153eeb0cb9e844d6d0f612b7c24db
FROM ${MULTICA_BACKEND_IMAGE} AS multica
FROM ${HERMES_IMAGE}
USER root
# Personal Kubernetes instances run as 1000; OpenSSH requires a passwd entry.
# Preserve the upstream hermes account used by the legacy s6 services.
RUN groupadd --gid 1000 hermes-personal \
 && useradd --uid 1000 --gid 1000 --home-dir /opt/data --no-create-home --shell /bin/sh hermes-personal
COPY runtimes/hermes/dashboard-owner.py runtimes/hermes/patch-dashboard-owner.py /usr/local/lib/
RUN /opt/hermes/.venv/bin/python /usr/local/lib/patch-dashboard-owner.py
COPY runtimes/hermes/terminal-policy.py runtimes/hermes/patch-terminal-policy.py /usr/local/lib/
RUN /opt/hermes/.venv/bin/python /usr/local/lib/patch-terminal-policy.py
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

# Generic runtime adapters and optional dependencies travel with the image.
COPY runtimes/hermes/requirements.lock /tmp/assistant-requirements.lock
RUN uv --no-config pip install --python /opt/hermes/.venv/bin/python --no-deps -r /tmp/assistant-requirements.lock \
 && HERMES_HOME=/opt/hermes-stack/plugin-home /opt/hermes/.venv/bin/hermes plugins install hindsight --ref 176f8c2de1369f569c489b831d143b78128b5535 --no-deps --no-enable
# Bundled backend plugins load for every profile, including ones created later.
COPY runtimes/hermes/plugins/managed_execution /opt/hermes/plugins/managed_execution
COPY config/managed-execution /etc/hermes
COPY runtimes/hermes/patch-terminal-scope.py /tmp/patch-terminal-scope.py
RUN /opt/hermes/.venv/bin/python /tmp/patch-terminal-scope.py /opt/hermes/tools/terminal_scope.py \
 && rm /tmp/patch-terminal-scope.py \
 && chmod -R a+rX /opt/hermes/plugins/managed_execution
COPY runtimes/hermes/native_files.py runtimes/hermes/runtime-init.py /usr/local/lib/
COPY runtimes/hermes/personal-init.py /usr/local/lib/personal-init.py
COPY runtimes/hermes/personal-run.py /usr/local/lib/personal-run.py
COPY runtimes/hermes/assistant-init /etc/cont-init.d/019-hermes-runtime
RUN chmod 755 /etc/cont-init.d/019-hermes-runtime \
 && chmod -R a+rX /opt/hermes-stack

RUN uv --no-config pip install --python /opt/hermes/.venv/bin/python ddgs==9.16.0
