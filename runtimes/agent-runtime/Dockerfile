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
RUN git config --system user.name 'Hermes Agent' \
 && git config --system user.email 'hermes@localhost' \
 && git config --system init.defaultBranch main
RUN chmod 755 /entrypoint.sh
WORKDIR /workspace
RUN mkdir -p /home/worker/hostkeys /etc/ssh/authorized_keys && chown worker:worker /home/worker/hostkeys
EXPOSE 2222
USER worker
ENTRYPOINT ["/entrypoint.sh"]
