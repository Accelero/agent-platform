# Operations image, distinct from the assistant and the restricted developer image.
FROM platform-tools AS tools
FROM execution-browser
USER root
COPY --from=tools /usr/local/bin/kubectl /usr/local/bin/flux /usr/local/bin/helm /usr/local/bin/gh /usr/local/bin/
RUN git config --system credential.https://github.com.helper '!/usr/local/bin/gh auth git-credential'
COPY ssh/environment-platform-operator.conf /etc/ssh/sshd_config.d/worker-env.conf
LABEL org.opencontainers.image.title="Hermes Platform Operator execution" \
      org.opencontainers.image.description="Session-scoped cluster and GitOps operations with a persistent infrastructure workspace"
ENV HERMES_EXECUTION_ROLE=platform-operator \
    KUBECONFIG=/opt/platform/agent/kubeconfig \
    GH_CONFIG_DIR=/home/worker/.config/gh \
    HELM_REGISTRY_CONFIG=/opt/platform/registry/config.json
USER worker
