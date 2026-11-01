"""Namespace-scoped Kubernetes execution; no host daemon or socket access.

The adapter observes, creates and deletes Pods by name and lists the Pods it
manages. It decides nothing: classification of a Pod is `parse_pod`, and the
Pod's lifecycle belongs to the planner.
"""
from __future__ import annotations

import json
import os
import subprocess
import tempfile
from datetime import datetime
from pathlib import Path
from typing import Protocol

from .domain.model import POD_NAME, Failure, Pod, PodState

SERVICE_ACCOUNT = Path('/var/run/secrets/kubernetes.io/serviceaccount')
MANAGED_BY = 'agent-platform-runtime-manager'
SSH_PORT = 2222

# Container waiting reasons that will not resolve by waiting.
IMAGE_REASONS = frozenset({'ErrImagePull', 'ImagePullBackOff', 'InvalidImageName',
                           'ErrImageNeverPull', 'RegistryUnavailable'})
START_REASONS = frozenset({'CrashLoopBackOff', 'CreateContainerConfigError',
                           'CreateContainerError', 'RunContainerError',
                           'PostStartHookError', 'StartError'})


class Cluster(Protocol):
    def observe(self, name: str) -> Pod | None: ...
    def create(self, name: str, environment: str, template: dict) -> None: ...
    def delete(self, name: str) -> None: ...
    def list_managed(self) -> list[Pod]: ...


def _waiting_failure(status: dict) -> Failure | None:
    """A reason in the containers' state that will not resolve by waiting.

    A worker's only container never finishes, so any termination of it is a
    failure; a completed init container is normal and is not looked at.
    """
    for container in status.get('containerStatuses', []):
        state = container.get('state', {})
        reason = state.get('waiting', {}).get('reason')
        if reason in IMAGE_REASONS:
            return Failure.IMAGE_UNAVAILABLE
        if reason in START_REASONS or state.get('terminated'):
            return Failure.START_FAILED
    for container in status.get('initContainerStatuses', []):
        if container.get('state', {}).get('waiting', {}).get('reason') in IMAGE_REASONS:
            return Failure.IMAGE_UNAVAILABLE
    return None


def _timestamp(value: str | None) -> float:
    if not value:
        return 0.0
    return datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()


def parse_pod(raw: dict) -> Pod:
    """Classify a Pod object.

    Unschedulable and slow Pods are `PENDING` without a failure: waiting may
    still succeed and the start deadline bounds it.
    """
    meta, status = raw.get('metadata', {}), raw.get('status', {})
    failure = _waiting_failure(status)
    name, created = meta.get('name', ''), _timestamp(meta.get('creationTimestamp'))
    if meta.get('deletionTimestamp'):
        return Pod(name, PodState.TERMINATING, created=created)
    if status.get('phase') in ('Failed', 'Succeeded'):
        return Pod(name, PodState.FAILED, failure=failure or Failure.START_FAILED, created=created)
    ready = any(c.get('type') == 'Ready' and c.get('status') == 'True'
                for c in status.get('conditions', []))
    if ready and status.get('podIP') and failure is None:
        endpoint = {'host': status['podIP'], 'port': SSH_PORT, 'user': 'worker'}
        return Pod(name, PodState.READY, endpoint=endpoint, created=created)
    return Pod(name, PodState.PENDING, failure=failure, created=created)


def render_pod(name: str, environment: str, template: dict, namespace: str,
               pull_secrets: list) -> dict:
    """The worker Pod. Its shape is pinned by the chart's admission policy."""
    spec = template['kubernetes']
    role = spec.get('role', 'unconfigured')
    pod = {
        'apiVersion': 'v1', 'kind': 'Pod',
        'metadata': {'name': name, 'namespace': namespace, 'labels': {
            'app.kubernetes.io/managed-by': MANAGED_BY,
            'hermes-execution': environment, 'hermes.stack/role': role}},
        'spec': {
            'restartPolicy': 'Never', 'automountServiceAccountToken': False,
            'enableServiceLinks': False,
            'serviceAccountName': spec.get('service_account', 'sandbox-worker'),
            'securityContext': {
                'runAsNonRoot': True, 'runAsUser': spec.get('uid', 1000),
                'runAsGroup': spec.get('gid', 1000),
                'seccompProfile': {'type': 'RuntimeDefault'}},
            'volumes': spec.get('volumes', []),
            'containers': [{
                'name': 'execution', 'image': template['image'],
                'imagePullPolicy': spec.get('image_pull_policy', 'IfNotPresent'),
                'ports': [{'containerPort': SSH_PORT}],
                'volumeMounts': spec.get('volume_mounts', []),
                'env': [{'name': key, 'value': str(value)}
                        for key, value in spec.get('environment', {}).items()],
                'securityContext': {'allowPrivilegeEscalation': False,
                                    'capabilities': {'drop': ['ALL']}},
                'resources': spec.get('resources', {
                    'requests': {'cpu': '100m', 'memory': '128Mi'},
                    'limits': {'cpu': '2', 'memory': '4Gi'}}),
                'readinessProbe': {'tcpSocket': {'port': SSH_PORT}, 'periodSeconds': 2}}]}}
    if pull_secrets:
        pod['spec']['imagePullSecrets'] = pull_secrets
    if spec.get('node_selector'):
        pod['spec']['nodeSelector'] = spec['node_selector']
    if spec.get('host_aliases'):
        pod['spec']['hostAliases'] = spec['host_aliases']
    return pod


class KubernetesCluster:
    def __init__(self, config: dict):
        self.config = config
        self.namespace = config.get('namespace', 'hermes-execution')
        self.kubeconfig = config.get('kubeconfig')
        self._credentials_directory = None
        host, port = os.environ.get('KUBERNETES_SERVICE_HOST'), os.environ.get('KUBERNETES_SERVICE_PORT')
        if not self.kubeconfig and not config.get('context') and host and port:
            if not all((SERVICE_ACCOUNT / name).is_file() for name in ('token', 'ca.crt')):
                raise RuntimeError('Runtime manager requires its projected service-account identity')
            self._credentials_directory = tempfile.TemporaryDirectory(prefix='runtime-manager-kube-')
            path = Path(self._credentials_directory.name) / 'config.json'
            address = '[' + host + ']' if ':' in host else host
            # Credentials stay in Kubernetes' rotating projected volume; this
            # private file holds only paths and the public endpoint.
            value = {'apiVersion': 'v1', 'kind': 'Config',
                     'clusters': [{'name': 'in-cluster', 'cluster': {
                         'server': 'https://' + address + ':' + port,
                         'certificate-authority': str(SERVICE_ACCOUNT / 'ca.crt')}}],
                     'users': [{'name': 'manager', 'user': {'tokenFile': str(SERVICE_ACCOUNT / 'token')}}],
                     'contexts': [{'name': 'manager', 'context': {
                         'cluster': 'in-cluster', 'user': 'manager', 'namespace': self.namespace}}],
                     'current-context': 'manager'}
            path.touch(mode=0o600)
            path.write_text(json.dumps(value))
            self.kubeconfig = str(path)

    def kubectl(self, *args, body=None):
        """One bounded kubectl call; failures carry a short, secret-free message."""
        command = ['kubectl']
        if self.kubeconfig:
            command += ['--kubeconfig', self.kubeconfig]
        elif self.config.get('context'):
            command += ['--context', self.config['context']]
        if self._credentials_directory:
            command += ['--cache-dir', str(Path(self._credentials_directory.name) / 'cache')]
        command += ['--namespace', self.namespace, '--request-timeout=20s', *args]
        result = subprocess.run(command, input=json.dumps(body) if body else None, text=True,
                                capture_output=True, timeout=30)
        if result.returncode:
            raise RuntimeError('Kubernetes execution operation failed: ' + result.stderr[:300])
        return json.loads(result.stdout) if result.stdout.strip().startswith('{') else result.stdout

    def _owned(self, name: str) -> dict | None:
        if not isinstance(name, str) or not POD_NAME.fullmatch(name):
            raise ValueError('Invalid managed Pod name')
        raw = self.kubectl('get', 'pod', name, '--ignore-not-found=true', '-o', 'json')
        if not raw:
            return None
        if (raw.get('metadata', {}).get('name') != name
                or raw['metadata'].get('labels', {}).get('app.kubernetes.io/managed-by') != MANAGED_BY):
            raise ValueError('Pod is not owned by the runtime manager')
        return raw

    def observe(self, name: str) -> Pod | None:
        raw = self._owned(name)
        return parse_pod(raw) if raw else None

    def create(self, name: str, environment: str, template: dict) -> None:
        """Create the Pod unless it exists; an existing one must be this environment's."""
        spec = template['kubernetes']
        raw = self._owned(name)
        if raw is not None:
            labels = raw['metadata'].get('labels', {})
            if (labels.get('hermes-execution') != environment
                    or labels.get('hermes.stack/role') != spec['role']):
                raise ValueError('Existing Pod does not match the approved environment')
            return
        pull_secrets = self.config.get('image_pull_secrets') or []
        self.kubectl('create', '-f', '-', '-o', 'json',
                     body=render_pod(name, environment, template, self.namespace, pull_secrets))

    def delete(self, name: str) -> None:
        """Ask for the Pod's deletion. Acceptance is not absence: the caller
        observes until the Pod is gone before it retires the generation."""
        raw = self._owned(name)
        if raw is not None and not raw.get('metadata', {}).get('deletionTimestamp'):
            self.kubectl('delete', 'pod', name, '--ignore-not-found=true', '--wait=false')

    def list_managed(self) -> list[Pod]:
        raw = self.kubectl('get', 'pods', '-l', 'app.kubernetes.io/managed-by=' + MANAGED_BY,
                           '-o', 'json')
        items = raw.get('items', []) if isinstance(raw, dict) else []
        return [parse_pod(item) for item in items
                if POD_NAME.fullmatch(item.get('metadata', {}).get('name', ''))]
