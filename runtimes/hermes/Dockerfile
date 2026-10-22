ARG HERMES_IMAGE=nousresearch/hermes-agent@sha256:d4da4a40cd7a28aba983775d9fd31d94cbf153eeb0cb9e844d6d0f612b7c24db
FROM ${HERMES_IMAGE}
USER root
# Personal Kubernetes instances run as 1000; OpenSSH requires a passwd entry.
# Preserve the upstream hermes account at UID 10000.
RUN groupadd --gid 1000 hermes-personal \
 && useradd --uid 1000 --gid 1000 --home-dir /opt/data --no-create-home --shell /bin/sh hermes-personal
COPY runtimes/hermes/dashboard-owner.py runtimes/hermes/patch-dashboard-owner.py /usr/local/lib/
RUN /opt/hermes/.venv/bin/python /usr/local/lib/patch-dashboard-owner.py
COPY runtimes/hermes/terminal-policy.py runtimes/hermes/patch-terminal-policy.py /usr/local/lib/
RUN /opt/hermes/.venv/bin/python /usr/local/lib/patch-terminal-policy.py
COPY runtimes/hermes/managed-exec /usr/local/bin/managed-exec
RUN mkdir -p /workspace \
 && chmod 755 /usr/local/bin/managed-exec

# Generic runtime adapters and optional dependencies travel with the image.
COPY runtimes/hermes/requirements.lock /tmp/assistant-requirements.lock
RUN uv --no-config pip install --python /opt/hermes/.venv/bin/python --no-deps -r /tmp/assistant-requirements.lock \
 && HERMES_HOME=/opt/hermes-stack/plugin-home /opt/hermes/.venv/bin/hermes plugins install hindsight --ref 176f8c2de1369f569c489b831d143b78128b5535 --no-deps --no-enable
# Bundled backend plugins load for every profile, including ones created later.
COPY runtimes/hermes/plugins/managed_execution /opt/hermes/plugins/managed_execution
RUN chmod -R a+rX /opt/hermes/plugins/managed_execution
COPY runtimes/hermes/native_files.py runtimes/hermes/runtime-init.py /usr/local/lib/
COPY runtimes/hermes/personal-init.py /usr/local/lib/personal-init.py
COPY runtimes/hermes/personal-run.py /usr/local/lib/personal-run.py
RUN chmod -R a+rX /opt/hermes-stack

RUN uv --no-config pip install --python /opt/hermes/.venv/bin/python ddgs==9.16.0
