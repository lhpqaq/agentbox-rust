#!/usr/bin/env python3
"""Build and package the current host's release executable from clean Git content."""
import hashlib
import io
import json
import pathlib
import subprocess
import tempfile
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]


def run(*args):
    return subprocess.check_output(args, cwd=ROOT)


def main():
    if run('git', 'status', '--porcelain', '--untracked-files=no').strip():
        raise SystemExit('Commit tracked changes before packaging a release.')
    commit = run('git', 'rev-parse', 'HEAD').decode().strip()
    build = subprocess.run(['cargo', 'build', '--locked', '--release', '--message-format=json'],
                           cwd=ROOT, capture_output=True, text=True)
    if build.returncode:
        raise SystemExit(build.stderr)
    executable = None
    for line in build.stdout.splitlines():
        record = json.loads(line)
        if record.get('reason') == 'compiler-artifact' and record.get('target', {}).get('name') == 'agentbox':
            if record.get('executable'):
                executable = pathlib.Path(record['executable'])
    if executable is None:
        raise SystemExit('Cargo did not report the agentbox executable.')
    with tempfile.TemporaryDirectory(prefix='ab-release-check-') as temp:
        missing = pathlib.Path(temp) / 'missing-state'
        diagnostic = subprocess.run([str(executable), 'doctor', '--home', str(missing)],
                                    capture_output=True, text=True, timeout=10)
        if diagnostic.returncode != 1 or missing.exists():
            raise SystemExit('Release diagnostic smoke check failed.')
        info = json.loads(diagnostic.stdout)
    version = info['client_version']
    if run(str(executable), '--version').decode().strip() != f'agentbox {version}':
        raise SystemExit('Release version check failed.')
    # Derive OS/architecture from the executable, not from a possibly cross-build host.
    name = f"agentbox-{version}-{info['platform']}-{info['architecture']}"
    binary = executable.read_bytes()
    metadata = {'version': version, 'commit': commit, 'platform': info['platform'],
                'architecture': info['architecture'], 'binary_sha256': hashlib.sha256(binary).hexdigest(),
                'validation': 'Built with Cargo.lock; version/read-only diagnostics checked. See repository CI for runtime validation.'}
    readme = f'''AgentBox {version} ({info['platform']} / {info['architecture']})

Run: ./agentbox --help
Start a shared terminal: ./agentbox attach
List sessions: ./agentbox sessions --active --table
Diagnose: ./agentbox doctor

The default private state directory is ~/.local/state/agentbox-rust.
Use the same --home directory for both operators. Ctrl-] detaches without closing
the shell. AgentBox executes with your account privileges and is not a sandbox.

Optional user-local installation:
  mkdir -p "$HOME/.local/bin"
  cp agentbox "$HOME/.local/bin/agentbox"
  export PATH="$HOME/.local/bin:$PATH"

Replacing the executable does not restart an existing broker. Finish active work
before deliberately restarting a broker: restarting terminates its shells.
This binary is not Developer ID signed or notarized. Build from source if your system's
application policy does not permit it; this package does not change those policies.

Source commit: {commit}
Repository: https://github.com/lhpqaq/agentbox-rust
'''
    entries = {'agentbox': (binary, 0o755), 'LICENSE': ((ROOT / 'LICENSE').read_bytes(), 0o644),
               'README.txt': (readme.encode(), 0o644),
               'BUILD-INFO.json': ((json.dumps(metadata, sort_keys=True, indent=2) + '\n').encode(), 0o644)}
    manifest = ''.join(f'{hashlib.sha256(data).hexdigest()}  {path}\n'
                       for path, (data, _) in sorted(entries.items()))
    entries['SHA256SUMS.txt'] = (manifest.encode(), 0o644)
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path, (data, mode) in sorted(entries.items()):
            entry = zipfile.ZipInfo(f'{name}/{path}', date_time=(1980, 1, 1, 0, 0, 0))
            entry.create_system = 3
            entry.external_attr = (0o100000 | mode) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(entry, data, compresslevel=9)
    content = output.getvalue()
    with zipfile.ZipFile(io.BytesIO(content)) as archive:
        if archive.testzip() is not None:
            raise SystemExit('Release ZIP CRC verification failed.')
    destination = ROOT / 'dist' / f'{name}.zip'
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(content)
    digest = hashlib.sha256(content).hexdigest()
    destination.with_suffix('.zip.sha256').write_text(f'{digest}  {destination.name}\n')
    print(json.dumps({'archive': str(destination), **metadata, 'archive_sha256': digest}, indent=2))


if __name__ == '__main__':
    main()
