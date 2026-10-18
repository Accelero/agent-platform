# Shared browser runtime for Developer, Codex and Platform Operator.
FROM execution-base
USER root
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends nodejs npm && rm -rf /var/lib/apt/lists/*
ARG PLAYWRIGHT_VERSION=1.58.2
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/playwright/browsers \
    NODE_PATH=/opt/playwright/node_modules
COPY install-playwright.sh /tmp/install-playwright.sh
RUN sh /tmp/install-playwright.sh && rm /tmp/install-playwright.sh
COPY ssh/environment-browser.conf /etc/ssh/sshd_config.d/worker-env.conf
USER worker
