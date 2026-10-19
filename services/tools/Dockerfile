FROM restic/restic:0.19.1@sha256:136600b6ff6843d61d355f7f71f460a166429f35de6fd11b568fece3c9a4d510 AS restic
FROM postgres:17.9-bookworm@sha256:47f917f7409eacd22fc5dfb1dee634e1b55cf0c01d1a7eb701be2227a03e0641 AS tools
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-venv ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && python3 -m venv /app/.venv \
 && /app/.venv/bin/pip install --no-cache-dir kubernetes==31.0.0 boto3==1.40.12 PyYAML==6.0.2
COPY --from=restic /usr/bin/restic /usr/local/bin/restic
COPY --from=multica-backend /usr/local/bin/multica-bootstrap /usr/local/bin/multica-bootstrap
COPY services/tools/platform_bootstrap/ /app/platform_bootstrap/
RUN mkdir -p /app
RUN rm -f /etc/ssl/private/ssl-cert-snakeoil.key /etc/ssl/certs/ssl-cert-snakeoil.pem
# Copy the cleaned filesystem so inherited layers cannot retain the unused key.
FROM scratch
COPY --from=tools / /
ENV PATH=/app/.venv/bin:/usr/lib/postgresql/17/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin PYTHONPATH=/app PYTHONDONTWRITEBYTECODE=1 HOME=/run/agent-tmp TMPDIR=/run/agent-tmp
WORKDIR /app
USER 1000:1000
ENTRYPOINT ["python"]
