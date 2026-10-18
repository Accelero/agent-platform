FROM restic/restic:0.19.1@sha256:136600b6ff6843d61d355f7f71f460a166429f35de6fd11b568fece3c9a4d510 AS restic
FROM postgres:17.9-bookworm@sha256:47f917f7409eacd22fc5dfb1dee634e1b55cf0c01d1a7eb701be2227a03e0641
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-venv ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && python3 -m venv /app/.venv \
 && /app/.venv/bin/pip install --no-cache-dir kubernetes==31.0.0 boto3==1.40.12 PyYAML==6.0.2
COPY --from=restic /usr/bin/restic /usr/local/bin/restic
RUN mkdir -p /app
ENV PATH=/app/.venv/bin:$PATH PYTHONPATH=/app PYTHONDONTWRITEBYTECODE=1 HOME=/run/agent-tmp TMPDIR=/run/agent-tmp
WORKDIR /app
USER 1000:1000
ENTRYPOINT ["python"]
