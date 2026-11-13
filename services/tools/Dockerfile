FROM restic/restic:0.19.1@sha256:136600b6ff6843d61d355f7f71f460a166429f35de6fd11b568fece3c9a4d510 AS restic
FROM postgres:18.7-trixie@sha256:9f4fd4aa918f52ae3d6f3b687f1de90d8f867cd4d520021545a9a50c120eae3b AS tools
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-venv ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && python3 -m venv /app/.venv \
 && /app/.venv/bin/pip install --no-cache-dir kubernetes==37.0.1 boto3==1.44.2 PyYAML==6.0.3
COPY --from=restic /usr/bin/restic /usr/local/bin/restic
COPY --from=multica-backend /usr/local/bin/multica-bootstrap /usr/local/bin/multica-bootstrap
COPY services/tools/platform_recovery/ /app/platform_recovery/
COPY charts/agent-platform/files/recovery.json /app/platform_recovery/contract.json
COPY services/tools/platform_bootstrap/ /app/platform_bootstrap/
RUN mkdir -p /app
RUN rm -f /etc/ssl/private/ssl-cert-snakeoil.key /etc/ssl/certs/ssl-cert-snakeoil.pem
# Copy the cleaned filesystem so inherited layers cannot retain the unused key.
FROM scratch
COPY --from=tools / /
ENV PATH=/app/.venv/bin:/usr/lib/postgresql/18/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin PYTHONPATH=/app PYTHONDONTWRITEBYTECODE=1 HOME=/run/agent-tmp TMPDIR=/run/agent-tmp
WORKDIR /app
USER 1000:1000
ENTRYPOINT ["python"]
