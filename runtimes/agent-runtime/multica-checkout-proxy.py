"""Expose only Multica's task-authenticated repository checkout endpoint."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
import json
import urllib.error
import urllib.request

class CheckoutProxy(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != '/repo/checkout':
            self.send_error(404)
            return
        token = self.headers.get('Authorization', '')
        if not token.startswith('Bearer mat_'):
            self.send_error(401)
            return
        try:
            length = int(self.headers.get('Content-Length', '0'))
        except ValueError:
            self.send_error(400)
            return
        if not 0 < length <= 1024 * 1024:
            self.send_error(413)
            return
        try:
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict):
                raise ValueError('object required')
        except (ValueError, UnicodeError):
            self.send_error(400)
            return
        # Linked worktrees would reference the private controller repo cache.
        payload['checkout_mode'] = 'isolated'
        request = urllib.request.Request(os.environ['MULTICA_CHECKOUT_UPSTREAM'] + self.path,
            data=json.dumps(payload).encode(), headers={
                'Authorization': token, 'Content-Type': 'application/json'})
        try:
            response = urllib.request.urlopen(request, timeout=300)
        except urllib.error.HTTPError as error:
            response = error
        except urllib.error.URLError:
            self.send_error(502)
            return
        with response:
            body = response.read()
            self.send_response(response.code)
            self.send_header('Content-Type', response.headers.get('Content-Type', 'application/json'))
            self.send_header('Content-Length', str(len(body)))
            # Native CLI retries repo lock contention only when both the
            # retry discriminator and delay survive every proxy hop.
            for header in ('X-Multica-Retryable', 'Retry-After'):
                if response.headers.get(header):
                    self.send_header(header, response.headers[header])
            self.end_headers()
            self.wfile.write(body)

    def log_message(self, *args):
        pass  # Never log tokens, task URLs or request bodies.

def main():
    ThreadingHTTPServer((os.environ['MULTICA_CHECKOUT_BIND'],
        int(os.environ['MULTICA_CHECKOUT_PORT'])), CheckoutProxy).serve_forever()


if __name__ == '__main__':
    main()
