import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from release_notes import parse_release


class ReleaseNotesTest(unittest.TestCase):
    def test_title_and_markdown_are_preserved(self):
        body = '## 功能\n\n- 锁定还原\n\n```sh\nrospm apply --dry-run\n```\n'
        self.assertEqual(
            parse_release('# v1.0.0 首次发布 $(echo literal)\n\n' + body, '1.0.0'),
            ('v1.0.0', '1.0.0', 'v1.0.0 首次发布 $(echo literal)', body),
        )

    def test_version_only_title(self):
        self.assertEqual(parse_release('# v0.0.1\n内容', '0.0.1')[:3],
                         ('v0.0.1', '0.0.1', 'v0.0.1'))

    def test_invalid_or_mismatched_version_and_empty_body(self):
        for text in ('# Changelog\n内容', '# v01.0.0\n内容',
                     '# v1.0.0-rc.1\n内容', '# v1.0.0/other\n内容',
                     '# v2.0.0\n内容', '# v1.0.0\n\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                parse_release(text, '1.0.0')

    def test_cli_outputs_and_stale_lock(self):
        script = Path(__file__).with_name('release_notes.py').resolve()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'Cargo.toml').write_text('[workspace.package]\nversion = "1.0.0"\n')
            (root / 'changelog.md').write_text('# v1.0.0 首次发布\n\n说明\n', encoding='utf-8')
            lock = '\n'.join(f'[[package]]\nname = "{name}"\nversion = "1.0.0"'
                             for name in ('rospm-cli', 'rospm-core'))
            (root / 'Cargo.lock').write_text(lock)
            output = root / 'output'
            env = dict(os.environ, GITHUB_OUTPUT=str(output))
            result = subprocess.run([sys.executable, str(script)], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output.read_text(encoding='utf-8'),
                             'tag=v1.0.0\nversion=1.0.0\ntitle=v1.0.0 首次发布\n')
            self.assertEqual((root / 'release-notes.md').read_text(encoding='utf-8'), '说明\n')
            output.unlink()
            (root / 'release-notes.md').unlink()
            (root / 'Cargo.lock').write_text(lock.replace('1.0.0', '0.0.1'))
            result = subprocess.run([sys.executable, str(script)], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Cargo.lock must contain', result.stderr)
            self.assertFalse(output.exists())
            self.assertFalse((root / 'release-notes.md').exists())


if __name__ == '__main__':
    unittest.main()
