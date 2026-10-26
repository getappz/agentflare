"""Local-only bridge checks; never contacts an AI provider."""
import http.client
import importlib.util
import io
import json
import os
import socket
import socketserver
import shutil
import ssl
import tempfile
import threading
import unittest
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'bridge', Path(__file__).resolve().parents[1] / 'assets' / 'claude_https.py')
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        body = b'{"service":"lean-ctx-proxy","status":"ok"}'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        self.server.received.append((self.path, dict(self.headers),
                                    self.rfile.read(int(self.headers['Content-Length']))))
        body = b'event: message_stop\ndata: {"type":"message_stop"}\n\n'
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class BridgeTest(unittest.TestCase):
    def test_other_ssl_errors_are_not_hidden_as_client_disconnects(self):
        handler = object.__new__(bridge.Frontend)
        with patch.object(BaseHTTPRequestHandler, 'handle', side_effect=ssl.SSLError('synthetic protocol failure')):
            with self.assertRaises(ssl.SSLError):
                handler.handle()

    def test_client_tls_eof_is_quiet_during_handle_and_finish(self):
        class EndedTlsWriter(io.BytesIO):
            def flush(self):
                raise ssl.SSLEOFError('synthetic client disconnect')

            def close(self):
                try:
                    self.flush()
                finally:
                    super().close()

        for stage in ('handle', 'finish'):
            with self.subTest(stage=stage):
                local, peer = socket.socketpair()
                handler = object.__new__(bridge.Frontend)
                context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                handler.connection = context.wrap_socket(local, server_side=True, do_handshake_on_connect=False)
                handler.wfile = EndedTlsWriter()
                handler.rfile = io.BytesIO()
                try:
                    if stage == 'handle':
                        with patch.object(BaseHTTPRequestHandler, 'handle', side_effect=ssl.SSLEOFError('synthetic client disconnect')):
                            handler.handle()
                    handler.finish()
                    self.assertTrue(handler.wfile.closed)
                    self.assertTrue(handler.rfile.closed)
                    self.assertEqual(handler.connection.fileno(), -1)
                finally:
                    if not handler.wfile.closed:
                        try:
                            handler.wfile.close()
                        except ssl.SSLEOFError:
                            pass
                    handler.rfile.close()
                    handler.connection.close()
                    peer.close()

    def test_signal_status_matches_shell_exit_convention(self):
        with patch.object(bridge.sys, 'platform', 'linux'):
            self.assertEqual(bridge.child_status(-15), 143)
            self.assertEqual(bridge.child_status(7), 7)
        with patch.object(bridge.sys, 'platform', 'win32'):
            self.assertEqual(bridge.child_status(-1073741819), -1073741819)

    def setUp(self):
        self.provider = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
        self.provider.received = []
        threading.Thread(target=self.provider.serve_forever, daemon=True).start()

    def tearDown(self):
        self.provider.shutdown()
        self.provider.server_close()

    def test_two_sessions_reuse_optimizer_and_preserve_login_cwd_args_and_parent_env(self):
        parent = os.environ.copy()
        ports = []
        directories = []

        def child(command, cwd, env):
            self.assertEqual(command, ['real-agentflare', '__agentflare-claude-native', '--resume', 'session with spaces'])
            self.assertEqual(cwd, Path.cwd())
            self.assertEqual(env['ANTHROPIC_AUTH_TOKEN'], 'synthetic-login')
            self.assertNotIn('ANTHROPIC_BASE_URL', env)
            for kind in ('CONFIG', 'DATA', 'STATE', 'CACHE'):
                key = f'LEAN_CTX_{kind}_DIR'
                self.assertEqual(env.get(key), parent.get(key))
            port = int(env['HTTPS_PROXY'].rsplit(':', 1)[1])
            ports.append(port)
            ca = Path(env['NODE_EXTRA_CA_CERTS'])
            directories.append(ca.parent)
            context = ssl.create_default_context(cafile=str(ca))
            conn = http.client.HTTPSConnection('127.0.0.1', port, context=context, timeout=3)
            conn.set_tunnel('api.anthropic.com', 443)
            conn.request('POST', '/v1/messages?beta=true', body=b'{"stream":true}',
                         headers={'Authorization': 'Bearer synthetic-login',
                                  'Content-Type': 'application/json'})
            response = conn.getresponse()
            self.assertEqual(response.status, 200)
            self.assertIn(b'message_stop', response.read())
            conn.close()
            path, headers, body = self.provider.received[-1]
            self.assertEqual(path, '/v1/messages?beta=true')
            self.assertEqual(headers['Authorization'], 'Bearer synthetic-login')
            self.assertEqual(body, b'{"stream":true}')
            return 7

        with patch.dict(os.environ, {'ANTHROPIC_AUTH_TOKEN': 'synthetic-login'}), \
                patch.object(bridge, 'PROXY_PORT', self.provider.server_port), \
                patch.object(bridge.subprocess, 'call', child), \
                patch.object(bridge.subprocess, 'run', side_effect=AssertionError('Must reuse optimizer')):
            self.assertEqual(bridge.run('real-agentflare', 'real-lean', ['--resume', 'session with spaces']), 7)
            self.assertEqual(bridge.run('real-agentflare', 'real-lean', ['--resume', 'session with spaces']), 7)
        self.assertEqual(os.environ, parent)
        self.assertEqual(len(self.provider.received), 2)
        self.assertTrue(all(not path.exists() for path in directories))
        for port in ports:
            with socket.socket() as probe:
                self.assertNotEqual(probe.connect_ex(('127.0.0.1', port)), 0)
        with closing(http.client.HTTPConnection('127.0.0.1', self.provider.server_port, timeout=3)) as conn:
            conn.request('GET', '/health')
            self.assertEqual(conn.getresponse().status, 200)

    def test_plaintext_requests_cannot_reach_optimizer(self):
        frontend = bridge.start_server(bridge.Frontend)
        frontend.lean_port = self.provider.server_port
        try:
            with closing(http.client.HTTPConnection('127.0.0.1', frontend.server_port, timeout=3)) as conn:
                conn.request('POST', '/v1/messages', body=b'{}')
                self.assertEqual(conn.getresponse().status, 403)
            self.assertFalse(self.provider.received)
        finally:
            frontend.shutdown()
            frontend.server_close()

    def test_unrelated_https_tunnels_remain_opaque(self):
        class Echo(socketserver.BaseRequestHandler):
            def handle(self):
                self.request.sendall(self.request.recv(64))
        echo = socketserver.TCPServer(('127.0.0.1', 0), Echo)
        threading.Thread(target=echo.serve_forever, daemon=True).start()
        frontend = bridge.start_server(bridge.Frontend)
        connect = socket.create_connection
        target_ports = []
        try:
            def destination(address, *args, **kwargs):
                if address[0] == 'opaque.invalid' and address[1] in (443, 8443):
                    target_ports.append(address[1])
                    address = echo.server_address
                return connect(address, *args, **kwargs)
            with patch.object(bridge.socket, 'create_connection', destination):
                for port in (443, 8443):
                    with closing(http.client.HTTPConnection('127.0.0.1', frontend.server_port, timeout=3)) as conn:
                        conn.set_tunnel('opaque.invalid', port)
                        conn.connect()
                        conn.sock.sendall(b'opaque TLS bytes')
                        self.assertEqual(conn.sock.recv(64), b'opaque TLS bytes')
                self.assertEqual(target_ports, [443, 8443])
        finally:
            frontend.shutdown()
            frontend.server_close()
            echo.shutdown()
            echo.server_close()

    def test_cold_start_detaches_without_inherited_pipe_hang(self):
        lean = shutil.which('lean-ctx')
        if not lean:
            self.skipTest('lean-ctx CLI not installed')
        with tempfile.TemporaryDirectory(prefix='agentflare-proxy-test-') as scratch:
            root = Path(scratch)
            env = {'LEAN_CTX_LIVE_PRICING': 'off', 'LEAN_CTX_NO_UPDATE_CHECK': '1'}
            for kind in ('CONFIG', 'DATA', 'STATE', 'CACHE'):
                directory = root / kind.lower()
                directory.mkdir()
                env[f'LEAN_CTX_{kind}_DIR'] = str(directory)
            with socket.socket() as probe:
                probe.bind(('127.0.0.1', 0))
                port = probe.getsockname()[1]
            with patch.dict(os.environ, env), patch.object(bridge, 'PROXY_PORT', port):
                try:
                    self.assertEqual(bridge.shared_proxy(lean), port)
                    with closing(http.client.HTTPConnection('127.0.0.1', port, timeout=3)) as conn:
                        conn.request('GET', '/health')
                        first_pid = json.loads(conn.getresponse().read())['pid']
                    self.assertEqual(bridge.shared_proxy(lean), port)
                    with closing(http.client.HTTPConnection('127.0.0.1', port, timeout=3)) as conn:
                        conn.request('GET', '/health')
                        self.assertEqual(json.loads(conn.getresponse().read())['pid'], first_pid)
                finally:
                    bridge.subprocess.run([lean, 'proxy', 'stop', f'--port={port}'],
                                          stdout=bridge.subprocess.DEVNULL,
                                          stderr=bridge.subprocess.DEVNULL, timeout=15)


if __name__ == '__main__':
    unittest.main()
