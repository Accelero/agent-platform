"""Install the exact Linux/amd64 tools recorded in downloads.lock.json."""
import hashlib
import io
import json
from pathlib import Path
import tarfile
import urllib.request

for name, spec in json.loads(Path('/tmp/downloads.lock.json').read_text()).items():
    data = urllib.request.urlopen(spec['url'], timeout=120).read()
    if hashlib.sha256(data).hexdigest() != spec['sha256']:
        raise RuntimeError(f'Checksum mismatch: {name}')
    if spec['kind'] == 'tar':
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
            data = archive.extractfile(spec['member']).read()
    target = Path('/usr/local/bin') / name
    target.write_bytes(data)
    target.chmod(0o755)
