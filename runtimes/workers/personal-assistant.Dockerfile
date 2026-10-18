# Official standalone Obsidian Headless; no desktop app or virtual display.
FROM node:22-bookworm-slim@sha256:43ac6c60b8f89723f746e8a92ce91abd5017e627ce1ddfe4238355d3a30b772c AS node
FROM execution-base AS obsidian
USER root
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/npm
ARG OBSIDIAN_HEADLESS_VERSION=0.0.14
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && npm install --global obsidian-headless@${OBSIDIAN_HEADLESS_VERSION}
FROM platform-tools AS tools
FROM execution-base
USER root
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends libstdc++6 && rm -rf /var/lib/apt/lists/*
COPY --from=tools /usr/local/bin/gh /usr/local/bin/gh
RUN git config --system credential.https://github.com.helper '!/usr/local/bin/gh auth git-credential'
ARG HIGGSFIELD_VERSION=1.1.25
COPY install-higgsfield.sh /tmp/install-higgsfield.sh
RUN sh /tmp/install-higgsfield.sh --tag "v${HIGGSFIELD_VERSION}" --prefix /usr/local --no-hf && rm /tmp/install-higgsfield.sh /usr/local/bin/higgs \
 && mkdir -p /home/worker/.config/higgsfield && chown worker:worker /home/worker/.config /home/worker/.config/higgsfield && chmod 700 /home/worker/.config/higgsfield
ENV GH_CONFIG_DIR=/home/worker/.config/gh \
    HIGGSFIELD_NO_UPDATE_CHECK=1 HIGGSFIELD_DISABLE_TELEMETRY=1
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=obsidian /usr/local/lib/node_modules/obsidian-headless /usr/local/lib/node_modules/obsidian-headless
RUN ln -s ../lib/node_modules/obsidian-headless/cli.js /usr/local/bin/ob \
 && mkdir -p /home/worker/.config/obsidian-headless \
 && chown worker:worker /home/worker/.config /home/worker/.config/obsidian-headless \
 && chmod 700 /home/worker/.config/obsidian-headless
LABEL org.opencontainers.image.title="Hermes Personal Assistant execution" \
      org.opencontainers.image.description="Personal Assistant toolchain with official Obsidian Headless"
COPY ssh/environment-personal-assistant.conf /etc/ssh/sshd_config.d/worker-env.conf
USER worker
