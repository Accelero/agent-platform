"""Load administrator-owned Kubernetes policy and derive its admission allowlist.

This config contains names and paths only. Credentials are separately provisioned
Secrets; profile/model defaults are seeded native state, not generated policy.
"""
from copy import deepcopy
from pathlib import PurePosixPath
import re

IDENTIFIER = r'[A-Za-z0-9][A-Za-z0-9_.-]{0,255}'
TASK = IDENTIFIER + '/' + IDENTIFIER


def anchored(path):
    return '^' + re.escape(str(path)) + '$'


def load_policy(native, *, image_tag='dev', registry='', uid=1000, gid=1000,
                namespace='hermes-execution', infra_namespace='hermes-system'):
    config = deepcopy(native)
    if config.get('backend') != 'kubernetes' or 'docker' in config:
        raise ValueError('Structural policy must use the Kubernetes backend')
    if uid <= 0 or gid <= 0:
        raise ValueError('Workers must run as non-root identities')
    config['kubernetes']['namespace'] = namespace
    policies = {}
    for template in config['templates'].values():
        if 'docker' in template or 'runtime_root' in template:
            raise ValueError('Legacy host and Docker mount wiring is unsupported')
        spec = template['kubernetes']
        role = spec['role']
        if role in policies:
            raise ValueError('Each worker role must have one explicit template')
        image = template['image']
        if image_tag is not None:
            name = image.rsplit('/', 1)[-1].split(':')[0].split('@')[0]
            image = (registry.rstrip('/') + '/' if registry else '') + name + ':' + image_tag
        template['image'] = image
        spec.update(uid=uid, gid=gid)
        volumes = {v['name']: v for v in spec['volumes']}
        if len(volumes) != len(spec['volumes']):
            raise ValueError('Duplicate worker volume name')
        mounts = []
        for mount in spec['volume_mounts']:
            source = volumes[mount['name']]
            rule = {'mountPathPattern': anchored(mount['mountPath']),
                    'readOnly': bool(mount.get('readOnly', False))}
            if 'persistentVolumeClaim' in source:
                rule['claim'] = source['persistentVolumeClaim']['claimName']
                if mount.get('subPath'):
                    rule['subPathPattern'] = anchored(mount['subPath'])
            elif 'secret' in source:
                secret = source['secret']
                if not secret.get('items'):
                    raise ValueError('Worker Secrets require explicit selected keys')
                rule.update(secret=secret['secretName'], keys=[i['key'] for i in secret['items']])
                if mount.get('subPath'):
                    rule['subPath'] = mount['subPath']
                if not rule['readOnly']:
                    raise ValueError('Worker Secret mounts must be read-only')
            elif 'emptyDir' in source:
                continue
            else:
                raise ValueError('Worker volumes must use selected PVCs or Secrets')
            mounts.append(rule)
        if template.get('workdir_scope'):
            scope = template['workdir_scope']
            if 'host_root' in scope or 'kubernetes_root' in scope:
                raise ValueError('Hermes workdirs require task-only PVCs')
            prefix = scope.get('kubernetes_prefix', '')
            prefix = re.escape(prefix.rstrip('/') + '/') if prefix else ''
            root = re.escape(scope['controller_root'].rstrip('/'))
            mounts.append({'claim': scope['kubernetes_claim'],
                'subPathPattern': '^' + prefix + TASK + '/workdir$',
                'mountPathPattern': '^' + root + '/' + TASK + '/workdir$',
                'readOnly': bool(scope['read_only'])})
            if scope.get('identity_kubernetes_claim'):
                identity = {'claim': scope['identity_kubernetes_claim'],
                    'mountPathPattern': '^' + root + '/' + TASK + '/hermes-home$', 'readOnly': False}
                if scope.get('identity_kubernetes_subpath'):
                    identity['subPathPattern'] = anchored(scope['identity_kubernetes_subpath'])
                mounts.append(identity)
        if template.get('profile_workspace'):
            scope = template['profile_workspace']
            # The grouped claim root is never allowed into an execution pod.
            segment = r'\.*[A-Za-z0-9_-][A-Za-z0-9_.-]{0,255}'
            subdir = segment + '(?:/' + segment + ')*'
            for root in (scope['mount_path'], scope['controller_root']):
                mounts.append({'claim': scope['kubernetes_claim'],
                    'subPathPattern': anchored(scope['kubernetes_subpath']),
                    'mountPathPattern': anchored(root), 'readOnly': False})
                mounts.append({'claim': scope['kubernetes_claim'],
                    'subPathPattern': '^' + re.escape(scope['kubernetes_subpath']) + '/' + subdir + '$',
                    'mountPathPattern': '^' + re.escape(root.rstrip('/')) + '/' + subdir + '$',
                    'readOnly': False})
        if template.get('runtime_guest_root'):
            root = re.escape(template['runtime_guest_root'].rstrip('/'))
            supported = {'.multica/workspaces': (TASK, root + r'/\.multica/workspaces/' + TASK),
                'environments': (IDENTIFIER + '/home', re.escape('/home/worker')),
                '.codex/multica-sessions': (TASK + '/' + IDENTIFIER,
                    root + r'/\.codex/multica-sessions/' + TASK + '/' + IDENTIFIER)}
            sources = template.get('runtime_kubernetes_mounts', [])
            if {s['path'] for s in sources} != set(supported):
                raise ValueError('Codex needs separate tasks, environments and session-store claims')
            if len({s['claim'] for s in sources}) != 3:
                raise ValueError('Codex task, home and history storage must be separate claims')
            for source in sources:
                subpath, target = supported[source['path']]
                prefix = source.get('prefix', '')
                prefix = re.escape(prefix.rstrip('/') + '/') if prefix else ''
                mounts.append({'claim': source['claim'], 'subPathPattern': '^' + prefix + subpath + '$',
                               'mountPathPattern': '^' + target + '$', 'readOnly': False})
        policies[role] = {'images': [image], 'mounts': mounts,
                         'environment': spec.get('environment', {})}
    return config, {'namespace': namespace, 'infraNamespace': infra_namespace, 'roles': policies}
