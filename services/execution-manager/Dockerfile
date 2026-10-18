FROM platform-tools AS tools
FROM python:3.12-slim-bookworm@sha256:54c85f3c47607a77f32adec749d3c81d1348bf25833671f512b26a9b6d778cb3
RUN pip install --no-cache-dir PyYAML==6.0.2
RUN mkdir -p /run/secrets/kubernetes.io/serviceaccount /run/execution-manager-auth /etc/execution-manager/tls
COPY --from=tools /usr/local/bin/kubectl /usr/local/bin/kubectl
WORKDIR /app
COPY *.py ./
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1 HOME=/tmp
USER 1000:1000
CMD ["python", "api.py"]
