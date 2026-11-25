"""Offline tests for deploy/software-library/receive-software.py:
python3 deploy/software-library/test_receive.py

The signature check is replaced by a plain parse (the `cryptography` package
is not needed); everything after it runs as on the server."""
import hashlib
import importlib.util
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('receive_software', HERE / 'receive-software.py')
receiver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(receiver)
receiver.verify = lambda manifest: json.loads(manifest['release'])


def archive(product, version, files):
    """What `publish.sh` streams: latest.json plus <version>/<files>."""
    release = {
        'product': product,
        'version': version,
        'files': [
            {'name': name, 'sha256': hashlib.sha256(data).hexdigest(), 'size': len(data)}
            for name, data in sorted(files.items())
        ],
    }
    manifest = json.dumps({'release': json.dumps(release), 'signature': '', 'key': ''}).encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w:gz') as tar:
        def add(name, data=None):
            info = tarfile.TarInfo(name)
            if data is None:
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            else:
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        add('./latest.json', manifest)
        add(f'./{version}')
        for name, data in files.items():
            add(f'./{version}/{name}', data)
    buffer.seek(0)
    return buffer


class ReceiveTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name)

    def tearDown(self):
        self.scratch.cleanup()

    def publish(self, product, version, files):
        receiver.receive(archive(product, version, files), self.root, product)

    def test_the_newest_cli_release_serves_its_installers_from_the_product_folder(self):
        self.publish('cli', '1.0.0', {
            'lynshen-x86_64-unknown-linux-gnu': b'one',
            'install.sh': b'#!/bin/sh\necho 1\n',
            'install.ps1': b'Write-Host 1\n',
            'SHA256SUMS': b'sums\n',
        })
        cli = self.root / 'cli'
        self.assertEqual((cli / 'install.sh').read_bytes(), b'#!/bin/sh\necho 1\n')
        self.assertEqual((cli / 'install.ps1').read_bytes(), b'Write-Host 1\n')
        self.assertEqual((cli / '1.0.0' / 'install.sh').read_bytes(), b'#!/bin/sh\necho 1\n')
        self.assertEqual((cli / 'install.sh').stat().st_mode & 0o777, 0o644)

        self.publish('cli', '1.1.0', {
            'lynshen-x86_64-unknown-linux-gnu': b'two',
            'install.sh': b'#!/bin/sh\necho 2\n',
            'install.ps1': b'Write-Host 2\n',
        })
        self.assertEqual((cli / 'install.sh').read_bytes(), b'#!/bin/sh\necho 2\n')
        self.assertEqual((cli / 'install.ps1').read_bytes(), b'Write-Host 2\n')
        self.assertEqual((cli / '1.0.0' / 'install.sh').read_bytes(), b'#!/bin/sh\necho 1\n')
        self.assertEqual(sorted(p.name for p in cli.iterdir() if p.name.startswith('.write-')), [])

    def test_an_older_release_changes_nothing(self):
        self.publish('cli', '1.1.0', {'install.sh': b'new\n', 'install.ps1': b'new\n'})
        with self.assertRaises(ValueError):
            self.publish('cli', '1.0.0', {'install.sh': b'old\n', 'install.ps1': b'old\n'})
        self.assertEqual((self.root / 'cli' / 'install.sh').read_bytes(), b'new\n')

    def test_a_tampered_installer_is_refused(self):
        stream = archive('cli', '1.0.0', {'install.sh': b'good\n'})
        data = stream.getvalue()
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as tar:
            members = [(m, tar.extractfile(m).read() if m.isfile() else None) for m in tar]
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode='w:gz') as tar:
            for member, content in members:
                if member.name.endswith('install.sh'):
                    content = b'evil\n'
                    member.size = len(content)
                tar.addfile(member, io.BytesIO(content) if content is not None else None)
        buffer.seek(0)
        with self.assertRaises(ValueError):
            receiver.receive(buffer, self.root, 'cli')
        self.assertFalse((self.root / 'cli' / 'install.sh').exists())

    def test_other_products_keep_their_files_in_the_version_folder(self):
        self.publish('skills', '2026.11.25', {'catalog.json': b'{}', 'install.sh': b'x\n'})
        self.assertFalse((self.root / 'skills' / 'install.sh').exists())
        self.assertTrue((self.root / 'skills' / '2026.11.25' / 'install.sh').is_file())


if __name__ == '__main__':
    unittest.main()
