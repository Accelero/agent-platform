"""Namespace-scoped Kubernetes execution; no host daemon or socket access."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

SERVICE_ACCOUNT = Path('/var/run/secrets/kubernetes.io/serviceaccount')


def ready_endpoint(pod):
    status = pod.get('status', {})
    if pod.get('metadata', {}).get('deletionTimestamp') or not status.get('podIP'):
        return None
    if any(c.get('type') == 'Ready' and c.get('status') == 'True' for c in status.get('conditions', [])):
        return {'host': status['podIP'], 'port': 2222, 'user': 'worker'}
    return None


class KubernetesBackend:
    def __init__(self, config):
        self.config = config
        self.namespace = config.get("namespace", "hermes-execution")
        self.kubeconfig = config.get('kubeconfig')
        self._credentials_directory = None
        host, port = os.environ.get('KUBERNETES_SERVICE_HOST'), os.environ.get('KUBERNETES_SERVICE_PORT')
        if not self.kubeconfig and not config.get('context') and host and port:
            if not all((SERVICE_ACCOUNT / name).is_file() for name in ('token', 'ca.crt')):
                raise RuntimeError('Runtime manager requires its projected service-account identity')
            self._credentials_directory = tempfile.TemporaryDirectory(prefix='runtime-manager-kube-')
            path = Path(self._credentials_directory.name) / 'config.json'
            address = '[' + host + ']' if ':' in host else host
            # Keep credentials in Kubernetes' rotating projected volume. This
            # private temporary config contains only paths and public endpoints.
            value = {'apiVersion': 'v1', 'kind': 'Config',
                'clusters': [{'name': 'in-cluster', 'cluster': {'server': 'https://' + address + ':' + port,
                    'certificate-authority': str(SERVICE_ACCOUNT / 'ca.crt')}}],
                'users': [{'name': 'manager', 'user': {'tokenFile': str(SERVICE_ACCOUNT / 'token')}}],
                'contexts': [{'name': 'manager', 'context': {'cluster': 'in-cluster', 'user': 'manager', 'namespace': self.namespace}}],
                'current-context': 'manager'}
            path.touch(mode=0o600)
            path.write_text(json.dumps(value))
            self.kubeconfig = str(path)

    def kubectl(self, *args, body=None):
        command = ["kubectl"]
        if self.kubeconfig:
            command += ["--kubeconfig", self.kubeconfig]
        elif self.config.get("context"):
            command += ["--context", self.config["context"]]
        if self._credentials_directory:
            command += ['--cache-dir', str(Path(self._credentials_directory.name) / 'cache')]
        command += ["--namespace", self.namespace, "--request-timeout=20s", *args]
        result = subprocess.run(command, input=json.dumps(body) if body else None, text=True, capture_output=True, timeout=30)
        if result.returncode:
            raise RuntimeError("Kubernetes execution operation failed: " + result.stderr[:300])
        return json.loads(result.stdout) if result.stdout.strip().startswith("{") else result.stdout

    def ensure(self, environment, generation, template, resource):
        name = f"hermes-exec-{environment}-{generation}"
        spec = template["kubernetes"]
        pod = self.get_pod(name)
        if pod and (pod['metadata']['labels'].get('hermes-execution') != environment
                    or pod['metadata']['labels'].get('hermes.stack/role') != spec['role']):
            raise ValueError('Existing Pod does not match the approved environment')
        if pod and pod.get("status", {}).get("phase") in ("Failed", "Succeeded"):
            if not self.remove({"name": name}):
                return {"name": name}, None
            pod = None
        if not pod:
            pod = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": name, "namespace": self.namespace,
                    "labels": {"app.kubernetes.io/managed-by": "agent-platform-runtime-manager", "hermes-execution": environment,
                               "hermes.stack/role": spec.get("role", "unconfigured")}},
                   "spec": {"restartPolicy": "Never", "automountServiceAccountToken": False,
                    "enableServiceLinks": False,
                    "serviceAccountName": spec.get("service_account", "sandbox-worker"),
                    "securityContext": {"runAsNonRoot": True, "runAsUser": spec.get("uid", 1000), "runAsGroup": spec.get("gid", 1000),
                                        "seccompProfile": {"type": "RuntimeDefault"}},
                    "volumes": spec.get("volumes", []), "containers": [{"name": "execution", "image": template["image"],
                      "imagePullPolicy": spec.get("image_pull_policy", "IfNotPresent"),
                      "ports": [{"containerPort": 2222}], "volumeMounts": spec.get("volume_mounts", []),
                      "env": [{"name": key, "value": str(value)} for key, value in spec.get("environment", {}).items()],
                      "securityContext": {"allowPrivilegeEscalation": False, "capabilities": {"drop": ["ALL"]}},
                      "resources": spec.get("resources", {"requests": {"cpu": "100m", "memory": "128Mi"},
                                                           "limits": {"cpu": "2", "memory": "4Gi"}}),
                      "readinessProbe": {"tcpSocket": {"port": 2222}, "periodSeconds": 2}}]}}
            if spec.get("node_selector"):
                pod["spec"]["nodeSelector"] = spec["node_selector"]
            if spec.get("host_aliases"):
                pod["spec"]["hostAliases"] = spec["host_aliases"]
            pod = self.kubectl("create", "-f", "-", "-o", "json", body=pod)
        # Kubernetes provisions asynchronously. The manager persists this
        # resource and clients retry acquisition without reserving a lease.
        return {"name": name}, ready_endpoint(pod)

    def is_running(self, resource):
        pod = self.get_pod(resource['name'])
        return bool(pod and ready_endpoint(pod))

    def get_pod(self, name):
        if not isinstance(name, str) or not re.fullmatch(r'hermes-exec-[0-9a-f]{24}-[0-9a-f]{12}', name):
            raise ValueError('Invalid managed Pod name')
        pod = self.kubectl('get', 'pod', name, '--ignore-not-found=true', '-o', 'json')
        if not pod:
            return None
        if (pod.get('metadata', {}).get('name') != name
                or pod['metadata'].get('labels', {}).get('app.kubernetes.io/managed-by') != 'agent-platform-runtime-manager'):
            raise ValueError('Pod is not owned by the runtime manager')
        return pod

    def remove(self, resource):
        pod = self.get_pod(resource['name'])
        if not pod:
            return True
        if not pod.get('metadata', {}).get('deletionTimestamp'):
            self.kubectl("delete", "pod", resource["name"], "--ignore-not-found=true", "--wait=false")
        # Acceptance is not absence. A later native snapshot acknowledges the
        # completed termination before any generation or mounted root is retired.
        return False
