"""Notice coverage and escaping checks without network or cargo-about."""
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('notices', Path(__file__).with_name('generate-notices.py'))
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class NoticeTests(unittest.TestCase):
    def fixture(self, name='example', version='1.0.0'):
        package = {'id': f'{name}@{version}', 'name': name, 'version': version, 'authors': ['A <author>']}
        return {'crates': [{'package': package, 'license': 'MIT'}],
                'licenses': [{'id': 'MIT', 'text': 'Copyright <holder>\n</pre><script>alert(1)</script>',
                              'source_path': '/source/LICENSE', 'used_by': [{'crate': package}]}]}

    def test_escapes_attribution_text_and_omits_local_paths(self):
        output = notices.render(self.fixture(), '<target>', 'hash', 'config', 'manifests')
        self.assertIn('example 1.0.0', output)
        self.assertIn('&lt;script&gt;', output)
        self.assertNotIn('<script>', output)
        self.assertNotIn('/source/LICENSE', output)
        self.assertIn('&lt;target&gt;', output)
        hostile = notices.render(self.fixture(), '--><script>bad</script>', 'hash', 'config', 'manifests')
        self.assertNotIn('<script>', hostile)
        self.assertIn('\\u003e', hostile.splitlines()[1])

    def test_missing_coverage_and_unknown_fallback_fail(self):
        data = self.fixture()
        data['licenses'][0]['source_path'] = None
        with self.assertRaisesRegex(ValueError, 'missing source license'):
            notices.render(data, 'target', 'hash', 'config', 'manifests')
        data = self.fixture()
        data['licenses'] = []
        with self.assertRaisesRegex(ValueError, 'missing license attribution'):
            notices.render(data, 'target', 'hash', 'config', 'manifests')

    def test_canonical_exceptions_are_version_scoped_and_visible(self):
        data = self.fixture('opensesame', '0.1.1')
        data['licenses'][0]['source_path'] = None
        output = notices.render(data, 'target', 'hash', 'config', 'manifests')
        self.assertIn('upstream license file unavailable', output)
        self.assertIn('A &lt;author&gt;', output)
        data = self.fixture('opensesame', '0.1.2')
        data['licenses'][0]['source_path'] = None
        with self.assertRaises(ValueError):
            notices.render(data, 'target', 'hash', 'config', 'manifests')

    def test_empty_text_unknown_package_and_duplicate_inventory_fail(self):
        for mutation in ('empty', 'unknown', 'duplicate'):
            data = copy.deepcopy(self.fixture())
            if mutation == 'empty':
                data['licenses'][0]['text'] = ''
            elif mutation == 'unknown':
                data['licenses'][0]['used_by'][0]['crate'] = {'id': 'unknown'}
            else:
                data['crates'].append(data['crates'][0])
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                notices.render(data, 'target', 'hash', 'config', 'manifests')


if __name__ == '__main__':
    unittest.main()
