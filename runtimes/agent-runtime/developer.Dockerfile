FROM platform-tools AS tools
FROM execution-browser
USER root
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential python3-venv python3-pip && rm -rf /var/lib/apt/lists/*
COPY --from=tools /usr/local/bin/helm /usr/local/bin/gh /usr/local/bin/
RUN git config --system credential.https://github.com.helper '!/usr/local/bin/gh auth git-credential'
ARG HIGGSFIELD_VERSION=1.1.25
COPY install-higgsfield.sh /tmp/install-higgsfield.sh
RUN sh /tmp/install-higgsfield.sh --tag "v${HIGGSFIELD_VERSION}" --prefix /usr/local --no-hf && rm /tmp/install-higgsfield.sh /usr/local/bin/higgs \
 && mkdir -p /home/worker/.config/higgsfield && chown worker:worker /home/worker/.config /home/worker/.config/higgsfield && chmod 700 /home/worker/.config/higgsfield
ENV GH_CONFIG_DIR=/home/worker/.config/gh \
    HIGGSFIELD_NO_UPDATE_CHECK=1 HIGGSFIELD_DISABLE_TELEMETRY=1
COPY ssh/environment-developer.conf /etc/ssh/sshd_config.d/worker-env.conf
USER worker
