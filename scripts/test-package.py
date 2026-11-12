#!/usr/bin/env python3
"""Verify deterministic packaging, executable modes, and local-state exclusion."""
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import tempfile
import zipfile

spec = importlib.util.spec_from_file_location('source_package', pathlib.Path(__file__).with_name('package.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def main():
    with tempfile.TemporaryDirectory(prefix='ab-package-') as temp:
        root = pathlib.Path(temp)
        repo = root / 'repo'
        repo.mkdir()

        def git(*args):
            return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.STDOUT)

        git('init', '-q')
        (repo / '.gitignore').write_text('.state/\n*.log\n')
        (repo / 'Cargo.lock').write_text('lockfile fixture\n')
        (repo / 'run.sh').write_text('#!/bin/sh\necho test\n')
        (repo / 'run.sh').chmod(0o755)
        (repo / '中文.md').write_text('测试\n')
        git('add', '.')
        git('-c', 'user.name=Package Test', '-c', 'user.email=test@example.invalid',
            '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture')
        (repo / '.state').mkdir()
        (repo / '.state/token').write_text('private-token-fixture')
        (repo / 'debug.log').write_text('private log')
        (repo / 'untracked.md').write_text('local notes')
        one, two = root / 'one.zip', root / 'two.zip'
        module.package(repo, one)
        module.package(repo, two)
        assert one.read_bytes() == two.read_bytes()
        with zipfile.ZipFile(one) as archive:
            assert archive.testzip() is None
            assert set(archive.namelist()) == {'.gitignore', 'Cargo.lock', 'run.sh', '中文.md',
                                               'SOURCE-MANIFEST.sha256', 'SOURCE-REVISION.json'}
            assert (archive.getinfo('run.sh').external_attr >> 16) & 0o777 == 0o755
            revision = json.loads(archive.read('SOURCE-REVISION.json'))
            assert revision['commit'] == git('rev-parse', 'HEAD').decode().strip()
            for line in archive.read('SOURCE-MANIFEST.sha256').decode().splitlines():
                digest, name = line.split('  ', 1)
                assert hashlib.sha256(archive.read(name)).hexdigest() == digest
        (repo / 'Cargo.lock').write_text('dirty\n')
        try:
            module.package(repo, root / 'dirty.zip')
        except ValueError:
            pass
        else:
            raise AssertionError('Dirty source tree was silently packaged')
        assert not (root / 'dirty.zip').exists()
        print('PASS: source package is reproducible, includes lockfile/modes/manifest, excludes local state, and rejects tracked changes')


if __name__ == '__main__':
    main()
