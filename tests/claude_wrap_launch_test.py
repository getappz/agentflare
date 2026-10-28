"""Windows real-process checks with a fake npm Claude, never a provider prompt."""
import http.client
import json
import os
import shutil
import subprocess
import sys
import tempfile
from contextlib import closing
from pathlib import Path

if os.name != 'nt':
    print('SKIP: Windows launcher checks')
    raise SystemExit(0)

binary = Path(sys.argv[1]).resolve()

proxy_cases = (False,)
try:
    if not shutil.which('lean-ctx'):
        raise RuntimeError('lean-ctx is not on PATH')
    # Cold startup is covered separately with an isolated port and state.
    with closing(http.client.HTTPConnection('127.0.0.1', 4444, timeout=1)) as conn:
        conn.request('GET', '/health')
        response = conn.getresponse()
        service = json.loads(response.read(4096))
        if response.status != 200 or not isinstance(service, dict) or service.get('service') != 'lean-ctx-proxy':
            raise RuntimeError('default port 4444 is not serving lean-ctx-proxy')
except (ImportError, OSError, http.client.HTTPException, ValueError, RuntimeError) as error:
    print(f'SKIP: proxy launcher fixtures require lean-ctx and a running default optimizer: {error}')
else:
    proxy_cases = (False, True)


def run(args, env):
    return subprocess.run([str(a) for a in args], env=env, capture_output=True,
                          text=True, encoding='utf-8', timeout=30)


with tempfile.TemporaryDirectory(prefix="agentflare wrapper's $fixture ") as scratch:
    root = Path(scratch)
    capture = root / 'capture.py'
    capture.write_text(
        "import os,sys,json\n"
        "print(json.dumps({'argv':sys.argv[1:],'proxy':os.environ.get('HTTPS_PROXY','').startswith('http://127.0.0.1:')}))\n"
        "raise SystemExit(7)\n", encoding='utf-8')
    (root / 'claude').write_text('#!/bin/sh\nexit 99\n', encoding='utf-8')
    (root / 'claude.cmd').write_text(
        f'@echo off\n"{sys.executable}" "{capture}" %*\nexit /b %errorlevel%\n', encoding='utf-8')
    env = os.environ.copy()
    env['PATH'] = str(root) + os.pathsep + env['PATH']
    env.pop('HTTPS_PROXY', None)
    env.pop('https_proxy', None)
    native = ['--version', 'argument with spaces', 'a&ver', 'unicode-\u03bb']

    # Even a faulty lookup can only issue --version to a real Claude install.
    direct = run([binary, '__agentflare-claude-native', *native], env)
    assert direct.returncode == 7, direct.stderr
    assert json.loads(direct.stdout) == {'argv': native, 'proxy': False}, direct.stdout

    ps_profile = root / 'profile.ps1'
    original = '# preserve unrelated setup\n'
    ps_profile.write_text(original, encoding='utf-8')
    installed = run([binary, 'wrap', 'claude-code', '--shell', 'powershell', '--profile', ps_profile], env)
    assert installed.returncode == 0, installed.stderr
    for proxy in proxy_cases:
        arguments = (['--with-proxy'] if proxy else []) + native
        encoded = json.dumps(arguments).replace("'", "''")
        profile_literal = str(ps_profile).replace("'", "''")
        script = f". '{profile_literal}'; $values=ConvertFrom-Json '{encoded}'; claude @values; $ok=$?; $code=$LASTEXITCODE; Write-Output ('STATUS|' + $ok + '|' + $code)"
        result = run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script], env)
        assert result.returncode == 0, result.stderr
        lines = result.stdout.strip().splitlines()
        assert json.loads(lines[0]) == {'argv': native, 'proxy': proxy}, result.stdout
        assert lines[-1] == 'STATUS|False|7', result.stdout
    removed = run([binary, 'unwrap', 'claude', '--shell', 'powershell', '--profile', ps_profile], env)
    assert removed.returncode == 0, removed.stderr
    assert ps_profile.read_text(encoding='utf-8') == original

    git = shutil.which('git')
    bash = Path(git).parent.parent / 'bin' / 'bash.exe' if git else None
    if bash and bash.is_file():
        bash_profile = root / 'profile.bash'
        bash_profile.write_text(original, encoding='utf-8')
        installed = run([binary, 'wrap', 'claude', '--shell', 'bash', '--profile', bash_profile], env)
        assert installed.returncode == 0, installed.stderr
        quoted = "'" + bash_profile.as_posix().replace("'", "'\"'\"'") + "'"
        for proxy in proxy_cases:
            option = '--with-proxy ' if proxy else ''
            result = run([bash, '--noprofile', '--norc', '-c', f'source {quoted}; claude {option}--version'], env)
            assert result.returncode == 7, result.stdout + result.stderr
            assert json.loads(result.stdout) == {'argv': ['--version'], 'proxy': proxy}, result.stdout
        removed = run([binary, 'unwrap', 'claude-code', '--shell', 'bash', '--profile', bash_profile], env)
        assert removed.returncode == 0, removed.stderr
        assert bash_profile.read_text(encoding='utf-8') == original

print(f'PASS: Windows npm resolution, native argument escaping, PowerShell status, unwrap; proxy fixtures {"passed" if True in proxy_cases else "skipped"}; no provider call')
