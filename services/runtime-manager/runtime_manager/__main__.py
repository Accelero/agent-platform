"""Runtime manager entry point: configuration, wiring, maintenance loop."""
from __future__ import annotations

import logging
import os
import signal
import threading
import time
from pathlib import Path

from .api import ExecutionAPI
from .server import create_server
from .service import Service

log = logging.getLogger(__name__)


def maintenance(service: Service, stop: threading.Event) -> None:
    """Reconcile every environment each interval and scan for orphans on a slower one."""
    last_scan = float('-inf')
    while not stop.is_set():
        try:
            service.maintain()
        except Exception:
            log.exception('Manager maintenance failed')
        if time.monotonic() - last_scan >= service.settings.orphan_scan:
            last_scan = time.monotonic()
            try:
                service.scan_orphans()
            except Exception:
                log.exception('Orphan scan failed')
        stop.wait(service.settings.maintenance_interval)


def serve(service: Service, api: ExecutionAPI, *, certfile: str, keyfile: str) -> None:
    server = create_server(api, certfile=certfile, keyfile=keyfile)
    stop = threading.Event()
    threading.Thread(target=maintenance, args=(service, stop), daemon=True).start()
    signal.signal(signal.SIGTERM, lambda *_: threading.Thread(target=server.shutdown).start())
    try:
        server.serve_forever()
    finally:
        stop.set()
        server.server_close()


def main() -> None:
    import yaml

    from .cluster import KubernetesCluster
    from .store import Store

    logging.basicConfig(level=logging.INFO)
    config = yaml.safe_load(Path(os.environ.get(
        'MANAGER_CONFIG', '/etc/runtime-manager/config.yaml')).read_text())
    if config.get('backend') != 'kubernetes':
        raise ValueError('Only the Kubernetes backend is supported')
    credentials = {name: Path(client['token_file']).read_text().strip()
                   for name, client in config['clients'].items()}
    service = Service(config, KubernetesCluster(config['kubernetes']),
                      Store(os.environ.get('MANAGER_DATABASE', '/state/manager.sqlite3')))
    serve(service, ExecutionAPI(service, credentials),
          certfile=os.environ.get('MANAGER_TLS_CERT_FILE', '/etc/runtime-manager/tls/tls.crt'),
          keyfile=os.environ.get('MANAGER_TLS_KEY_FILE', '/etc/runtime-manager/tls/tls.key'))


if __name__ == '__main__':
    main()
