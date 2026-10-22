ARG MULTICA_BACKEND_IMAGE=ghcr.io/multica-ai/multica-backend@sha256:60dc856bc44c01a3c73e8153e211be6c22cc6cbb66dde8b20e01de5b6e75e41f
FROM ${MULTICA_BACKEND_IMAGE} AS multica
FROM ubuntu:24.04
ARG HOST_UID=1000
ARG HOST_GID=1000
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends openssh-server python3 python3-yaml git curl jq ca-certificates ripgrep \
 && rm -rf /var/lib/apt/lists/* \
 && rm -f /etc/ssh/ssh_host_* /etc/ssl/private/ssl-cert-snakeoil.key /etc/ssl/certs/ssl-cert-snakeoil.pem
RUN userdel -r ubuntu 2>/dev/null || true
RUN groupadd -g ${HOST_GID} worker && useradd -m -u ${HOST_UID} -g worker -s /bin/bash worker && echo 'worker:disabled-password' | chpasswd && mkdir -p /run/sshd /home/worker/.ssh /workspace && chown -R worker:worker /home/worker /workspace
COPY sshd_config /etc/ssh/sshd_config
COPY ssh/environment-base.conf /etc/ssh/sshd_config.d/worker-env.conf
COPY entrypoint.sh /entrypoint.sh
COPY multica-checkout-proxy.py /usr/local/lib/multica-checkout-proxy.py
COPY --from=multica /app/multica /usr/local/bin/multica
COPY --from=multica /app/LICENSE /app/NOTICE /usr/local/share/multica/
RUN git config --system user.name 'Hermes Agent' \
 && git config --system user.email 'hermes@localhost' \
 && git config --system init.defaultBranch main
RUN chmod 755 /entrypoint.sh
WORKDIR /workspace
RUN mkdir -p /home/worker/hostkeys /etc/ssh/authorized_keys && chown worker:worker /home/worker/hostkeys
ENV MULTICA_CHECKOUT_UPSTREAM=http://hermes:19515 \
    MULTICA_CHECKOUT_BIND=127.0.0.1 \
    MULTICA_CHECKOUT_PORT=19514
EXPOSE 2222
USER worker
ENTRYPOINT ["/entrypoint.sh"]
