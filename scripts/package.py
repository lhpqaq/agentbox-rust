#!/usr/bin/env python3
"""Create a deterministic source ZIP from clean, committed Git content."""
import argparse
import hashlib
import io
import json
import pathlib
import subprocess
import tarfile
import zipfile


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args])


def package(repo, destination):
    if git(repo, 'status', '--porcelain', '--untracked-files=no').strip():
        raise ValueError('Tracked changes are not committed. Commit them before packaging.')
    commit = git(repo, 'rev-parse', 'HEAD').decode().strip()
    archive = git(repo, 'archive', '--format=tar', commit)
    entries = {}
    with tarfile.open(fileobj=io.BytesIO(archive)) as source:
        for member in source:
            if member.isdir():
                continue
            path = pathlib.PurePosixPath(member.name)
            if path.is_absolute() or '..' in path.parts or '\n' in member.name or '\r' in member.name:
                raise ValueError(f'Unsafe archive path: {member.name!r}')
            if not member.isfile():
                raise ValueError(f'Only regular source files are packaged: {member.name!r}')
            entries[member.name] = (source.extractfile(member).read(), 0o755 if member.mode & 0o111 else 0o644)
    for name in ['SOURCE-MANIFEST.sha256', 'SOURCE-REVISION.json']:
        if name in entries:
            raise ValueError(f'Reserved generated filename is already tracked: {name}')
    entries['SOURCE-REVISION.json'] = (json.dumps({'commit': commit, 'source': 'git HEAD',
        'validation': 'Packaging validates archive integrity, not build or runtime behavior.'},
        sort_keys=True, indent=2).encode() + b'\n', 0o644)
    manifest = ''.join(f'{hashlib.sha256(data).hexdigest()}  {name}\n'
                       for name, (data, _) in sorted(entries.items()))
    entries['SOURCE-MANIFEST.sha256'] = (manifest.encode(), 0o644)
    destination = pathlib.Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Construct in memory first: failed validation must not leave a partial artifact.
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as target:
        for name, (data, mode) in sorted(entries.items()):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            target.writestr(info, data, compresslevel=9)
    content = output.getvalue()
    with zipfile.ZipFile(io.BytesIO(content)) as check:
        if check.testzip() is not None:
            raise ValueError('ZIP CRC verification failed')
    destination.write_bytes(content)
    digest = hashlib.sha256(content).hexdigest()
    destination.with_suffix(destination.suffix + '.sha256').write_text(f'{digest}  {destination.name}\n')
    return {'archive': str(destination), 'commit': commit, 'files': len(entries), 'sha256': digest}


def main():
    root = pathlib.Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, default=root / 'dist/AgentBox-Rust-source.zip')
    args = parser.parse_args()
    try:
        print(json.dumps(package(root, args.output), indent=2))
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'Packaging failed: {error}\n')


if __name__ == '__main__':
    main()
