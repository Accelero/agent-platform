FROM platform-tools AS tools
FROM python:3.14-slim-trixie@sha256:9aaa474097d094040649f2162a627c0049b408f52138d980916788d80f04802f
RUN pip install --no-cache-dir PyYAML==6.0.3
RUN mkdir -p /run/secrets/kubernetes.io/serviceaccount /run/runtime-manager-auth /etc/runtime-manager/tls
COPY --from=tools /usr/local/bin/kubectl /usr/local/bin/kubectl
WORKDIR /app
COPY runtime_manager/ ./runtime_manager/
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1 HOME=/tmp
USER 1000:1000
CMD ["python", "-m", "runtime_manager"]
