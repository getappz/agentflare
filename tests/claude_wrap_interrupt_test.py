"""Use an isolated console/process group; never signal the user's terminal."""
import ctypes
import http.client
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def wait_until(predicate, process, timeout=15):
    deadline = time.monotonic() + timeout
    while not predicate():
        if process.poll() is not None:
            raise AssertionError(f'wrapper exited prematurely: {process.returncode}')
        if time.monotonic() >= deadline:
            raise AssertionError('fixture did not become ready')
        time.sleep(.02)


def controller(binary, mode):
    signal.signal(signal.SIGINT, lambda *_: None)
    ready = Path(os.environ['AF_INTERRUPT_READY'])
    stop = Path(os.environ['AF_INTERRUPT_STOP'])
    received = Path(os.environ['AF_INTERRUPT_RECEIVED'])
    native = ['--version']
    if mode == 'raw':
        native.append('\ud800' if os.name == 'nt' else os.fsdecode(b'\xff'))
    prefix = ['__agentflare-claude-native'] if mode == 'raw' else ['wrap', 'claude', '--launch', '--']
    if mode == 'proxy':
        prefix.append('--with-proxy')
    process = subprocess.Popen([binary, *prefix, *native])
    try:
        wait_until(ready.is_file, process)
        payload = ready.read_text().splitlines()
        encoded = [arg.encode('utf-8', 'surrogatepass').hex() if os.name == 'nt' else os.fsencode(arg).hex() for arg in native]
        assert payload[1:] == encoded, payload
        if os.name != 'nt' and mode != 'proxy':
            assert int(payload[0]) == process.pid, 'native Unix launch must exec rather than leave a wrapper parent'
        if mode != 'raw':
            if os.name == 'nt':
                assert ctypes.windll.kernel32.GenerateConsoleCtrlEvent(0, 0)
            else:
                os.killpg(os.getpgrp(), signal.SIGINT)
            wait_until(received.is_file, process)
            time.sleep(.5)
            assert process.poll() is None, 'Ctrl-C ended the wrapper while Claude kept running'
        stop.write_text('exit')
        assert process.wait(timeout=10) == 7
    finally:
        stop.write_text('exit')
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        if os.name == 'nt' and ready.is_file():
            kernel = ctypes.WinDLL('kernel32', use_last_error=True)
            kernel.OpenProcess.restype = ctypes.c_void_p
            kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
            kernel.CloseHandle.argtypes = [ctypes.c_void_p]
            handle = kernel.OpenProcess(0x100000, False, int(ready.read_text().splitlines()[0]))
            if handle:
                kernel.WaitForSingleObject(handle, 5000)
                kernel.CloseHandle(handle)


if len(sys.argv) > 1 and sys.argv[1] == '--controller':
    controller(sys.argv[2], sys.argv[3])
    raise SystemExit(0)

binary = str(Path(sys.argv[1]).resolve())
bad_arg = '\ud800' if os.name == 'nt' else os.fsdecode(b'\xff')
probe = subprocess.run([binary, '--help', bad_arg], capture_output=True)
failures = []
if probe.returncode == 101 or b'panicked' in probe.stderr:
    failures.append('ordinary CLI dispatch panics on non-Unicode arguments')
with tempfile.TemporaryDirectory(prefix='agentflare-interrupt-') as scratch:
    root = Path(scratch)
    fixture = root / ('claude.exe' if os.name == 'nt' else 'claude')
    compile_result = subprocess.run(
        ['rustc', '--edition=2024', str(Path(__file__).parent / 'fixtures' / 'claude_interrupt.rs'), '-o', str(fixture)],
        capture_output=True, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    assert compile_result.returncode == 0, compile_result.stderr.decode(errors='replace')
    modes = ['plain', 'raw']
    if shutil.which('lean-ctx'):
        conn = http.client.HTTPConnection('127.0.0.1', 4444, timeout=1)
        try:
            conn.request('GET', '/health')
            if json.loads(conn.getresponse().read()).get('service') == 'lean-ctx-proxy':
                modes.append('proxy')
        except (OSError, ValueError, http.client.HTTPException):
            pass
        finally:
            conn.close()
    if 'proxy' not in modes:
        print('SKIP: proxy interrupt fixture requires installed dependencies and an existing default proxy')
    else:
        lean = Path(shutil.which('lean-ctx'))
        try:
            os.link(lean, root / lean.name)
        except OSError:
            shutil.copyfile(lean, root / lean.name)
    for mode in modes:
        env = os.environ.copy()
        env.update(PATH=str(root) + os.pathsep + env['PATH'], AF_INTERRUPT_READY=str(root / f'{mode}.ready'),
                   AF_INTERRUPT_RECEIVED=str(root / f'{mode}.received'), AF_INTERRUPT_STOP=str(root / f'{mode}.stop'))
        if mode == 'proxy':
            env['PATH'] = str(root)
            assert not shutil.which('python', path=env['PATH'])
            assert not shutil.which('python3', path=env['PATH'])
        options = {'start_new_session': True} if os.name != 'nt' else {'creationflags': subprocess.CREATE_NEW_CONSOLE}
        if os.name == 'nt':
            startup = subprocess.STARTUPINFO()
            startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
            startup.wShowWindow = 0
            options['startupinfo'] = startup
        result = subprocess.run([sys.executable, str(Path(__file__).resolve()), '--controller', binary, mode],
                                env=env, capture_output=True, timeout=40, **options)
        if result.returncode:
            failures.append(mode + ': ' + result.stderr.decode(errors='replace'))
assert not failures, '\n'.join(failures)
print('PASS: raw OS arguments and terminal Ctrl-C preserve Claude lifetime and exit status; no provider calls')
