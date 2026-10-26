"""Experimental Claude HTTPS transport, embedded by agentflare.

shortcut: HTTP/1.1 and 2 MB requests; use a maintained transport before
relying on long sessions or Remote Control. Child tools inherit proxy env.
"""
import datetime as dt
import http.client
import json
import os
import select
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID

PROXY_PORT = 4444
HOST = 'api.anthropic.com'
HOP_HEADERS = {'host', 'connection', 'proxy-authorization', 'proxy-connection',
               'transfer-encoding', 'content-length'}
CLIENT_DISCONNECTS = (ConnectionResetError, BrokenPipeError, ssl.SSLEOFError, ssl.SSLZeroReturnError)


def certs(root):
    now = dt.datetime.now(dt.timezone.utc)
    ca_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'Agentflare temporary Claude CA')])
    ca = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
          .public_key(ca_key.public_key()).serial_number(x509.random_serial_number())
          .not_valid_before(now - dt.timedelta(minutes=1)).not_valid_after(now + dt.timedelta(days=1))
          .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
          .sign(ca_key, hashes.SHA256()))
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    leaf_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, HOST)])
    leaf = (x509.CertificateBuilder().subject_name(leaf_name).issuer_name(name)
            .public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - dt.timedelta(minutes=1)).not_valid_after(now + dt.timedelta(days=1))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName(HOST)]), critical=False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .sign(ca_key, hashes.SHA256()))
    (root / 'ca.pem').write_bytes(ca.public_bytes(serialization.Encoding.PEM))
    (root / 'leaf.pem').write_bytes(leaf.public_bytes(serialization.Encoding.PEM))
    key_path = root / 'leaf.key'
    key_path.write_bytes(key.private_bytes(serialization.Encoding.PEM,
                                         serialization.PrivateFormat.PKCS8,
                                         serialization.NoEncryption()))
    key_path.chmod(0o600)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(root / 'leaf.pem', key_path)
    context.set_alpn_protocols(['http/1.1'])
    return context


class Frontend(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass  # Credentials, URLs and payloads are never logged by the bridge.

    def handle(self):
        try:
            super().handle()
        except CLIENT_DISCONNECTS:
            pass

    def finish(self):
        try:
            super().finish()
        except CLIENT_DISCONNECTS:
            pass
        finally:
            for stream in (self.wfile, self.rfile):
                if not stream.closed:
                    try:
                        stream.close()
                    except CLIENT_DISCONNECTS:
                        pass
            if isinstance(self.connection, ssl.SSLSocket):
                self.connection.close()

    def do_CONNECT(self):
        self.close_connection = True
        if self.path == HOST + ':443':
            self.send_response(200, 'Connection established')
            self.end_headers()
            self.wfile.flush()
            try:
                self.connection = self.server.tls_context.wrap_socket(self.connection, server_side=True)
                self.rfile = self.connection.makefile('rb')
                self.wfile = self.connection.makefile('wb')
                self.handle_one_request()
            except (ssl.SSLError, OSError):
                pass
            self.close_connection = True
            return
        # Other HTTPS remains encrypted end-to-end, including network tools.
        try:
            host, port = self.path.rsplit(':', 1)
            port = int(port)
            if not 1 <= port <= 65535:
                raise ValueError('Invalid tunnel port')
            upstream = socket.create_connection((host.strip('[]'), port), timeout=30)
        except (OSError, ValueError):
            self.send_error(502, 'Cannot establish HTTPS tunnel')
            return
        with upstream:
            self.send_response(200, 'Connection established')
            self.end_headers()
            self.wfile.flush()
            try:
                while True:
                    readable, _, _ = select.select([self.connection, upstream], [], [], 60)
                    if not readable:
                        return
                    for source in readable:
                        chunk = source.recv(65536)
                        if not chunk:
                            return
                        target = upstream if source is self.connection else self.connection
                        target.sendall(chunk)
            except OSError:
                return

    def do_GET(self):
        self.relay()

    def do_POST(self):
        self.relay()

    def relay(self):
        self.close_connection = True
        if not isinstance(self.connection, ssl.SSLSocket):
            self.send_error(403)
            return
        try:
            size = int(self.headers.get('Content-Length', '0'))
            if size < 0 or len(self.headers.get_all('Content-Length', [])) > 1:
                raise ValueError('Invalid length')
        except ValueError:
            self.send_error(400)
            return
        if size > 2_000_000 or self.headers.get('Transfer-Encoding'):
            self.send_error(413)
            return
        if not self.path.startswith('/') or self.path.startswith('//'):
            self.send_error(400)
            return
        body = self.rfile.read(size) if size else None
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP_HEADERS}
        model_request = self.command == 'POST' and self.path.split('?', 1)[0] == '/v1/messages'
        conn = (http.client.HTTPConnection('127.0.0.1', self.server.lean_port, timeout=300)
                if model_request else http.client.HTTPSConnection(HOST, timeout=60))
        started = False
        try:
            conn.request(self.command, self.path, body=body, headers=headers)
            response = conn.getresponse()
            self.send_response(response.status)
            for key, value in response.getheaders():
                if key.lower() not in HOP_HEADERS | {'server', 'date'}:
                    self.send_header(key, value)
            self.send_header('Transfer-Encoding', 'chunked')
            self.send_header('Connection', 'close')
            self.end_headers()
            started = True
            while chunk := response.read1(65536):
                self.wfile.write(f'{len(chunk):x}\r\n'.encode() + chunk + b'\r\n')
                self.wfile.flush()
            self.wfile.write(b'0\r\n\r\n')
            self.wfile.flush()
        except (OSError, http.client.HTTPException):
            if not started:
                self.send_error(502, 'Upstream transport failed')
        finally:
            conn.close()


def start_server(handler):
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def child_status(code):
    return 128 - code if code < 0 and sys.platform != 'win32' else code


def shared_proxy(lean):
    def ready():
        try:
            with closing(http.client.HTTPConnection('127.0.0.1', PROXY_PORT, timeout=1)) as conn:
                conn.request('GET', '/health')
                response = conn.getresponse()
                service = json.loads(response.read(4096))
                if response.status != 200 or service.get('service') != 'lean-ctx-proxy':
                    raise RuntimeError(f'Port {PROXY_PORT} is occupied by another service')
                return True
        except (OSError, http.client.HTTPException):
            return False

    if ready():
        return PROXY_PORT
    env = os.environ.copy()
    for key in list(env):
        if key.upper().endswith('_PROXY'):
            env.pop(key)
    env.update(LEAN_CTX_PROXY_MODE='cache', LEAN_CTX_NO_UPDATE_CHECK='1')
    # Detached Windows children keep pipe handles open: never capture their pipes.
    subprocess.run([lean, 'proxy', 'start', '--detach', f'--port={PROXY_PORT}'], env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20,
                   creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    for _ in range(50):
        if ready():
            return PROXY_PORT
        time.sleep(.1)
    raise RuntimeError('Run lean-ctx proxy start --detach; default proxy did not become ready')


def run(runner, lean, args):
    base = os.environ.get('ANTHROPIC_BASE_URL', '').rstrip('/')
    if base and base != 'https://' + HOST:
        raise RuntimeError('--with-proxy currently supports the default Anthropic HTTPS endpoint')
    port = shared_proxy(lean)
    with tempfile.TemporaryDirectory(prefix='agentflare-claude-https-') as scratch:
        root = Path(scratch)
        tls = certs(root)
        env = os.environ.copy()
        previous_ca = env.get('NODE_EXTRA_CA_CERTS')
        if previous_ca:
            with (root / 'ca.pem').open('ab') as output:
                output.write(b'\n' + Path(previous_ca).read_bytes())
        for key in list(env):
            if key.upper().endswith('_PROXY') or key == 'ANTHROPIC_BASE_URL':
                env.pop(key)
        frontend = start_server(Frontend)
        frontend.tls_context, frontend.lean_port = tls, port
        env.update(HTTPS_PROXY=f'http://127.0.0.1:{frontend.server_port}',
                   NODE_EXTRA_CA_CERTS=str(root / 'ca.pem'), NO_PROXY='127.0.0.1,localhost')
        try:
            print(f'agentflare: HTTPS bridge -> shared lean-ctx on 127.0.0.1:{port}', file=sys.stderr)
            print('Monitor: lean-ctx proxy status', file=sys.stderr)
            return child_status(subprocess.call(
                [runner, '__agentflare-claude-native', *args], cwd=Path.cwd(), env=env))
        finally:
            frontend.shutdown()
            frontend.server_close()


if __name__ == '__main__':
    try:
        raise SystemExit(run(sys.argv[1], sys.argv[2], sys.argv[3:]))
    except KeyboardInterrupt:
        raise SystemExit(130)
    except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
        print(f'agentflare: {error}', file=sys.stderr)
        raise SystemExit(1)
