"""Compare the Rust reference and Go CLI using identical isolated filesystem paths."""
import argparse
import fcntl
import json
import os
import random
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class Parity(unittest.TestCase):
    rust: Path
    go: Path
    fixtures = 0
    invocations = 0

    @classmethod
    def tearDownClass(cls):
        print(f'Parity exercised {cls.fixtures} isolated configurations and {cls.invocations} CLI invocations.')

    def compare(self, scenario):
        type(self).fixtures += 1
        with tempfile.TemporaryDirectory(prefix="skills-sync-parity-") as temporary:
            root = Path(temporary).resolve() / "case"
            observations = []
            for binary in [self.rust, self.go]:
                shutil.rmtree(root, ignore_errors=True)
                root.mkdir()
                self.root = root
                self.binary = binary
                self.env = dict(os.environ, HOME=str(root / "home"), XDG_STATE_HOME=str(root / "state"), GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1", GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.com", GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.com", GIT_AUTHOR_DATE="2025-01-01T00:00:00Z", GIT_COMMITTER_DATE="2025-01-01T00:00:00Z")
                self.steps = []
                scenario()
                observations.append(self.steps)
            self.assertEqual(len(observations[0]), len(observations[1]))
            for rust, go in zip(*observations):
                with self.subTest(command=rust[0]):
                    self.assertEqual(rust[:3], go[:3])
                    self.assertEqual(rust[3].keys(), go[3].keys())
                    for path in rust[3]:
                        with self.subTest(path=path):
                            self.assertEqual(rust[3][path], go[3][path])

    def write(self, path, content):
        full = self.root / path
        full.parent.mkdir(parents=True, exist_ok=True)
        full.write_text(content)

    def manifest(self, sources, prefix="store", targets="['target-a', 'target-b']"):
        setting = f"prefix = {json.dumps(prefix)}\n" if prefix is not None else ""
        self.write("skills.toml", f"version = 1\n{setting}targets = {targets}\n{sources}\n")

    def run_cli(self, *args, success=True):
        type(self).invocations += 1
        result = subprocess.run(['skills-sync', *args], executable=self.binary, cwd=self.root, env=self.env, capture_output=True, timeout=30)
        if success is not None:
            self.assertEqual(result.returncode == 0, success, f"{self.binary} {args}\n{result.stdout.decode(errors='replace')}\n{result.stderr.decode(errors='replace')}")
        files = {}
        for path in sorted(self.root.rglob('*')):
            relative = path.relative_to(self.root)
            if '.git' in relative.parts or 'repos' in relative.parts:
                continue
            if path.is_symlink():
                files[str(relative)] = ['link', os.readlink(path)]
            elif path.is_file():
                files[str(relative)] = ['file', path.read_bytes(), bool(path.stat().st_mode & 0o111)]
            elif path.is_dir():
                files[str(relative)] = ['directory']
        self.steps.append((args, result.returncode, result.stdout if result.returncode == 0 else b'', files))
        return result

    def test_cli_contract(self):
        def scenario():
            for args in [[], ['--version'], ['--help'], ['help'], ['help', 'sync'], ['sync', '--help'], ['init', '--help'], ['update', '--help'], ['list', '--help'], ['patch', '--help'], ['patch', 'generate', '--help'], ['patch', 'help', 'generate'], ['bogus'], ['--manifest'], ['sync', '--locked', '--locked'], ['list', '--offline'], ['patch', 'generate'], ['init', 'extra']]:
                self.run_cli(*args, success=None)
            for args in [['--manifest', 'a', '--manifest', 'b', 'sync'], ['-m', 'a', 'sync', '-m', 'b'], ['-hm'], ['sync', '-hm'], ['--manifest=', 'sync'], ['--help', '--manifest'], ['patch', 'generate', 'x', 'a', 'b', '--output=']]:
                self.run_cli(*args, success=None)
            self.run_cli('init')
            self.run_cli('init', success=False)
        self.compare(scenario)

    def test_local_lifecycle(self):
        def scenario():
            self.write('local/nested/alpha/SKILL.md', '# Alpha\n\nhello\n')
            self.write('local/.hidden/beta/SKILL.md', 'beta\n')
            self.manifest("[sources.local]\npath = 'local'")
            self.run_cli('sync', '--dry-run')
            self.run_cli('sync', '--offline')
            self.run_cli('sync', '--locked', '--offline')
            self.run_cli('list', '--json')
            self.run_cli('list')
            self.write('local/nested/alpha/SKILL.md', 'updated\n')
            self.run_cli('sync', '--locked', success=False)
            self.run_cli('sync')
            self.manifest("[sources.local]\npath = 'local'\nskills = ['nested/**']\nrename = {'nested/alpha' = 'renamed'}", targets="['target-c']")
            self.run_cli('sync')
            self.manifest('[sources]')
            self.run_cli('sync')
        self.compare(scenario)

    def test_commands_lifecycle(self):
        def scenario():
            self.write('local/alpha/SKILL.md', 'alpha\n')
            source = "[sources.x]\npath = 'local'"
            self.manifest(source)
            self.run_cli('sync')
            self.manifest('commands = []\n' + source)
            self.run_cli('sync', '--locked', '--offline')

            commands = "commands = ['printf a >> runs', 'printf b >> runs']\n"
            self.manifest(commands + source)
            self.run_cli('sync', '--locked', success=False)
            self.run_cli('sync', '--dry-run')
            self.run_cli('update', '--dry-run')
            self.run_cli('sync')
            self.run_cli('sync', '--locked', '--offline')
            self.run_cli('update', 'x')
            self.run_cli('list', '--json')

            for changed in [commands.replace('printf b', 'printf c'), "commands = ['printf b >> runs', 'printf a >> runs']\n", 'commands = []\n']:
                self.manifest(changed + source)
                self.run_cli('sync', '--locked', success=False)

            self.manifest("commands = ['printf x >> runs', 'test -f allow || exit 7', 'touch third']\n" + source)
            self.run_cli('sync', success=False)
            self.write('allow', '')
            self.run_cli('sync', '--locked', '--offline')

            self.manifest(commands + '[sources]')
            self.run_cli('sync')
            self.run_cli('update')
        self.compare(scenario)

    def git(self, *args):
        result = subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgSign=false', *args], cwd=self.root / 'upstream', env=self.env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        return result.stdout.decode().strip()

    def repository(self):
        self.write('upstream/plugins/tools/skills/alpha/SKILL.md', 'alpha v1\n')
        self.write('upstream/deep/beta/SKILL.md', 'beta v1\n')
        self.git('init', '-b', 'main')
        self.git('add', '.')
        self.git('commit', '-m', 'initial')

    def test_git_lifecycle_and_targeted_update(self):
        def scenario():
            self.repository()
            self.git('tag', 'v1')
            sources = "[sources.a]\nrepo = 'upstream'\nref = 'main'\nskills = ['plugins/**']\n[sources.b]\nrepo = 'upstream'\nskills = ['deep/*']"
            self.manifest(sources)
            self.run_cli('sync', '--offline', success=False)
            self.run_cli('sync')
            self.write('upstream/plugins/tools/skills/alpha/SKILL.md', 'alpha v2\n')
            self.write('upstream/deep/beta/SKILL.md', 'beta v2\n')
            self.git('add', '.')
            self.git('commit', '-m', 'second')
            self.run_cli('sync', '--locked', '--offline')
            self.run_cli('update', 'a', '--dry-run')
            self.run_cli('update', 'a')
            self.run_cli('list', '--json')
            self.run_cli('update')
            self.run_cli('update', 'missing', success=False)
            self.manifest(sources.replace("ref = 'main'", "ref = 'v1'"))
            self.run_cli('sync')
            for name in ['store', 'target-a', 'target-b']:
                shutil.rmtree(self.root / name)
            self.run_cli('sync', '--locked')
            shutil.rmtree(self.root / 'upstream')
            self.run_cli('sync', '--locked', '--offline')
        self.compare(scenario)

    def test_prefix_resolution_and_manifest_aliases(self):
        for xdg in [None, '', 'relative', 'absolute']:
            with self.subTest(xdg=xdg):
                def scenario():
                    self.write('local/alpha/SKILL.md', 'alpha\n')
                    if xdg is None:
                        self.env.pop('XDG_STATE_HOME')
                    elif xdg != 'absolute':
                        self.env['XDG_STATE_HOME'] = xdg
                    self.manifest("[sources.x]\npath = 'local'", prefix=None)
                    (self.root / 'alias.toml').symlink_to('skills.toml')
                    self.run_cli('--manifest', 'alias.toml', 'sync')
                    self.run_cli('sync', '--manifest', str(self.root / 'skills.toml'), '--locked', '--offline')
                    self.run_cli('list', '--json')
                self.compare(scenario)

    def test_selection_and_source_boundaries(self):
        for selector in ["['*']", "['**/alpha', '**/beta']", "['plugins/{a,b}/**']", "['plugins/[ab]/skills/*']", "['plugins/?/skills/**', 'plugins/a/skills/alpha']", "['missing']", "['[']"]:
            with self.subTest(selector=selector):
                def scenario():
                    self.write('local/plugins/a/skills/alpha/SKILL.md', 'alpha\n')
                    self.write('local/plugins/b/skills/beta/SKILL.md', 'beta\n')
                    self.write('local/.hidden/gamma/SKILL.md', 'gamma\n')
                    self.write('local/.git/ignored/SKILL.md', 'ignored\n')
                    self.write('local/plugins/a/skills/alpha/child/SKILL.md', 'child\n')
                    self.manifest(f"[sources.x]\npath = 'local'\nskills = {selector}")
                    self.run_cli('sync', success=selector not in ["['missing']", "['[']"])
                self.compare(scenario)

    def test_source_permissions_and_symlinks(self):
        for target in ['../helper', '../missing', '../../../outside', '/tmp', 'link']:
            with self.subTest(target=target):
                def scenario():
                    self.write('local/a/SKILL.md', 'alpha\n')
                    self.write('local/helper', '#!/bin/sh\necho hello\n')
                    (self.root / 'local/helper').chmod(0o755)
                    (self.root / 'local/a/link').symlink_to(target)
                    self.manifest("[sources.x]\npath = 'local'")
                    self.run_cli('sync', success=target == '../helper')
                self.compare(scenario)

    def test_collision_and_ownership_failures(self):
        def scenario():
            self.write('local/a/SKILL.md', 'a\n')
            self.write('local/nested/a/SKILL.md', 'second\n')
            self.manifest("[sources.x]\npath = 'local'")
            self.run_cli('sync', success=False)
            self.manifest("[sources.x]\npath = 'local'\nrename = {'nested/a' = 'b'}")
            self.write('target-b/b/SKILL.md', 'unmanaged\n')
            self.run_cli('sync', success=False)
            shutil.rmtree(self.root / 'target-b/b')
            self.run_cli('sync')
            (self.root / 'target-a/a').unlink()
            self.run_cli('sync', '--locked')
            (self.root / 'target-a/a').unlink()
            (self.root / 'target-a/a').symlink_to(self.root / 'local/a')
            self.run_cli('sync', success=False)
        self.compare(scenario)

    def test_recovery_and_cross_version_replay(self):
        def scenario():
            self.write('local/a/SKILL.md', 'a\n')
            self.manifest("[sources.x]\npath = 'local'")
            self.run_cli('sync')
            other = self.go if self.binary == self.rust else self.rust
            result = subprocess.run([str(other), 'sync', '--locked', '--offline'], cwd=self.root, env=self.env, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            state = json.loads((self.root / 'store/state.json').read_text())
            lock = (self.root / 'skills.lock').read_text()
            after = json.loads(json.dumps(state))
            destination = next(iter(state['links'].values()))
            extra = self.root / 'target-a/extra'
            after['links'][str(extra)] = destination
            extra.symlink_to(destination)
            self.write('store/transaction.json', json.dumps(dict(before=state, after=after, lock_before=lock)))
            self.write('skills.lock', 'interrupted')
            self.run_cli('list', success=False)
            self.run_cli('sync', '--dry-run')
            self.run_cli('sync', '--locked')
            self.write('store/transaction.json', json.dumps(dict(before=state, after=after, lock_before=lock)))
            extra.symlink_to(self.root / 'local/a')
            self.run_cli('sync', success=False)
        self.compare(scenario)

    def test_concurrent_sync_and_modified_cache(self):
        def scenario():
            self.write('local/a/SKILL.md', 'a\n')
            self.manifest("[sources.x]\npath = 'local'")
            self.run_cli('sync')
            with (self.root / 'store/sync.lock').open('r+') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.run_cli('sync', success=False)
            (self.root / 'target-a/a/SKILL.md').write_text('tampered\n')
            self.run_cli('sync', success=False)
        self.compare(scenario)

    def patch(self, operation, file='a/SKILL.md'):
        self.write('change.skillpatch.toml', f'version = 1\nfile = {json.dumps(file)}\n{operation}\n')
        self.manifest("[sources.x]\npath = 'local'\npatches = ['change.skillpatch.toml']")

    def replacement(self, find, replace='chosen', section=None, minimum=1, maximum=None, words=False):
        if maximum is None:
            maximum = minimum
        return f'[[operations]]\nop = "replace_text"\nsection = {json.dumps(section or [])}\nfind = {json.dumps(find, ensure_ascii=False)}\nreplace = {json.dumps(replace, ensure_ascii=False)}\nwhole_words = {str(words).lower()}\nexpect = {{min = {minimum}, max = {maximum}}}'

    def test_markdown_protection_and_unicode(self):
        cases = [
            ('# Title\n\n## Tools\nUse old\n  tool here.\n\n### Nested\nold tool too.\n', 'old tool', 2, False, ['Tools']),
            ('---\nname: target\n---\n# target\n\n## Prose\ntarget `target` [target](target) *target* &target; \\*target\\*\n\n```\ntarget\n```\n\n<div>target</div>\n', 'target', 4, False, ['Prose']),
            ('cafe\u0301 cafe café 你好世界 Привет приветствие\n', 'cafe', 1, True, []),
            ('cafe\u0301 cafe café 你好世界 Привет приветствие\n', '你好', 1, True, []),
            ('cafe\u0301 cafe café 你好世界 Привет приветствие\n', 'Привет', 1, True, []),
            ('a\u1c89b\n', '\u1c89', 0, True, []),
            ('target\x00target\n', 'target\x00target', 1, False, []),
            ('&notit; notit\n', 'notit', 2, False, []),
            ('&#55296; 55296\n', '55296', 2, False, []),
            ('## &notit;\ntarget\n', 'target', 1, False, ['&notit;']),
            ('## &#128;\ntarget\n', 'target', 1, False, ['\u0080']),
            ('---\r\nname: target\r\n---\r\n# Title\r\n\r\nBefore <span>target</span> target after.\r\n\r\n- item <span>target</span> target\r\n\r\nSeparate target prose.\r\n', 'target', 1, False, []),
            ('<https://target.example> <target@example.com> target prose.\n', 'target', 1, False, []),
            ('\ufeff---  \nname: target\n---\n\ntarget prose.\n', 'target', 1, False, []),
            ('| target |\n| --- |\n| target |\n\nSeparate target.\n', 'target', 1, False, []),
            ('target &amp; target\n', 'target', 2, False, []),
            ('- target\n- **target**\n\n> target\n', 'target', 3, False, []),
            ('## A\ntext\n', 'missing', 0, False, ['Absent']),
        ]
        for title in ['cafe\u0301', 'a\u200db', 'a\u00adb', 'a\x00b', 'a\u00a0b', 'a\u200bb', 'a\ue000b']:
            cases.append(('## ' + title + '\ntarget\n', 'target', 1, False, [title]))
        for source, find, count, words, section in cases:
            with self.subTest(source=source, find=find):
                def scenario():
                    self.write('local/a/SKILL.md', source)
                    self.patch(self.replacement(find, section=section, minimum=count, words=words))
                    self.run_cli('sync', '--dry-run')
                    self.run_cli('sync')
                    self.run_cli('sync', '--locked', '--offline')
                self.compare(scenario)

    def test_markdown_rejects_unsafe_edits(self):
        cases = [('plain target text\n', self.replacement('target', value)) for value in ['*marked*', '`code`', '[link](url)', 'new\n\n## Heading', '&amp;', '\\*']]
        cases += [
            ('aaaa\n', self.replacement('aa', 'b', minimum=3)),
            ('## Same\nx\n## Same\nx\n', self.replacement('x', 'y', section=['Same'], minimum=0, maximum=2)),
            ('target &amp; target\n', self.replacement('target target')),
            ('target  \ntarget\n', self.replacement('target target')),
            ('---\nname: target\n', self.replacement('target')),
        ]
        for source, operation in cases:
            with self.subTest(source=source, operation=operation):
                def scenario():
                    self.write('local/a/SKILL.md', source)
                    self.patch(operation)
                    self.run_cli('sync', success=False)
                self.compare(scenario)

    def test_markdown_insertion(self):
        for ending in ['\n', '\r\n']:
            for content in ['body\n\n### Deep\n```rust\ncode\n```\n', '## Wrong\n', '```\nunterminated\n', '<!-- unclosed\n']:
                with self.subTest(ending=ending, content=content):
                    def scenario():
                        source = '# Title\n\n## A\ntext\n\n### Child\nchild\n\n## B\nother\n'.replace('\n', ending)
                        body = content.replace('\n', ending)
                        self.write('local/a/SKILL.md', source)
                        operation = f'[[operations]]\nop = "insert_section"\nafter = ["A"]\nheading = "New"\ncontent = {json.dumps(body)}'
                        self.patch(operation)
                        self.run_cli('sync', '--dry-run', success=content.startswith('body'))
                        self.run_cli('sync', success=content.startswith('body'))
                    self.compare(scenario)

    def test_generation_exact_bytes_and_replay(self):
        cases = [
            ('# T\n\n## A\nUse old tool here. Other old tool.\n', '# T\n\n## A\nUse new tool here. Other old tool.\n'),
            ('# T\n\n## A\nUse a simple tool here.\n', '# T\n\n## A\nUse a tool here.\n'),
            ('# T\r\n\r\n## A\r\nUse café tools.\r\n', '# T\r\n\r\n## A\r\nUse local café tools.\r\n'),
            ('Unique plain old sentence.\n', 'Unique plain new sentence.\n'),
            ('# Title\n\n## A\nUse old tools, keep all these intervening words unchanged, verify once today.\n', '# Title\n\n## A\nUse new tools, keep all these intervening words unchanged, verify twice today.\n'),
            ('# Title\n\n## A\nBody.\n\n## B\nOther.\n', '# Title\n\n## A\nBody.\n\n## Extra\n\nraw body\n\n### Nested\n```\nraw code\n```\n\n## B\nOther.\n'),
        ]
        for source, edited in cases:
            with self.subTest(source=source, edited=edited):
                def scenario():
                    self.write('local/a/SKILL.md', source)
                    self.write('edited.md', edited)
                    self.manifest("[sources.x]\npath = 'local'")
                    self.run_cli('sync')
                    self.run_cli('patch', 'generate', 'x', 'a/SKILL.md', 'edited.md', '--output', 'generated.skillpatch.toml')
                    self.manifest("[sources.x]\npath = 'local'\npatches = ['generated.skillpatch.toml']")
                    self.run_cli('sync')
                    self.assertEqual((self.root / 'target-a/a/SKILL.md').read_bytes(), edited.encode())
                    self.run_cli('sync', '--locked', '--offline')
                self.compare(scenario)

    def test_markdown_corpus(self):
        prose = [
            'target', ' target ', 'target\ncontinued target', 'target\n\tcontinued target',
            'target  \ntarget', 'target\\\ntarget', '*target*', '**target**', '***target***',
            '_target_', '__target__', '~~target~~', 'a ~~target~~ b', '~~target~',
            'target `target` target', 'target ``target ` target`` target',
            '[target](https://example.com "target")', '[target][ref]\n\n[ref]: /target',
            '[target][missing]', '![target](target)', '[![target](target)](target)',
            '&target; &amp; target', 'target&#32;target', 'target\\*target\\*target',
            '\\# target', '\\[target\\](url)', 'a<span>target</span> target',
            '<https://target.example> target', '<target@example.com> target',
            '- target\n- target', '- target\n\n  target', '- before\n\n  <span>target</span>\n\n  target',
            '1. target\n2. target', '> target\n> target', '> - target',
            '    target', '\t target', '```text\ntarget\n```\ntarget',
            '<!-- target -->\n\ntarget', '<div>target</div>\n\ntarget',
            'target\n===\n\ntarget', 'target\n---\n\ntarget',
            '## target\n\ntarget', '  ## target ##\n\ntarget',
            '| a | b |\n| --- | --- |\n| target | target |\n\ntarget',
            'target\u00a0target', 'target\u2003target', 'target\u2028target',
            'target &quot;target&quot;', 'target\x00target',
            '#\n\ntarget', '### ###\n\ntarget', '# *target* `target`\n\ntarget',
            '> ## target\n> target\n\ntarget', '- ## target\n  target\n',
            'target\n- - -\n\ntarget', 'target [target](<a b>) target',
            '[target](url "title") target', '[target]: /url\n\n[target] target',
            'target &#0; target', 'target &NotEqualTilde; target',
            'a*target*b', 'a_target_b', '~~target~~target', '~~~~target~~~~',
        ]
        for document in prose:
            with self.subTest(document=document):
                def scenario():
                    self.write('local/a/SKILL.md', document + '\n')
                    self.patch(self.replacement('target', 'chosen', minimum=0, maximum=100))
                    self.run_cli('sync', '--dry-run', success=None)
                    self.run_cli('sync', success=None)
                self.compare(scenario)

    def test_randomized_selection_and_patch_inputs(self):
        randomizer = random.Random(572103709)
        for case in range(35):
            paths = randomizer.sample(['alpha', 'nested/beta', '.hidden/gamma', 'plugins/a/skills/delta', 'plugins/b/skills/epsilon'], randomizer.randint(1, 5))
            body = randomizer.choice(['plain target prose.\n', '# T\n\n## A\nUse target tools.\n', 'target `target` *target*\n'])
            find = randomizer.choice(['target', 'missing', 'tools'])
            replacement = randomizer.choice(['chosen', 'café', '"quoted"', '\\u2028', 'local\u2028tools'])
            with self.subTest(case=case, paths=paths, replacement=replacement):
                def scenario():
                    for path in paths:
                        self.write('local/' + path + '/SKILL.md', body)
                    self.patch(self.replacement(find, replacement, minimum=0, maximum=10), file=paths[0] + '/SKILL.md')
                    self.run_cli('sync', '--dry-run', success=None)
                    self.run_cli('sync', success=None)
                self.compare(scenario)

    def test_serialization_unusual_paths(self):
        for name in ['quote"name', "apostrophe'name", 'back\\slash', 'space name', 'unicode-é', 'line\nbreak', 'literal\\u2028', 'separator\u2028name']:
            with self.subTest(name=name):
                def scenario():
                    self.write('local/' + name + '/SKILL.md', 'plain\n')
                    encoded = json.dumps(name, ensure_ascii=False)
                    self.manifest('[sources."source.id"]\npath="local"\nrename={' + encoded + '="selected"}')
                    self.run_cli('sync', success=None)
                    self.run_cli('sync', '--locked', '--offline', success=None)
                self.compare(scenario)

    def test_manifest_validation_corpus(self):
        sources = [
            '[sources]', '[sources.x]', "[sources.x]\npath='local'\nrepo='local'",
            "[sources.x]\npath='local'\nref='main'", "[sources.x]\npath='local'\nskills=[]",
            "[sources.x]\npath='local'\nunknown=true", "[sources.'bad name']\npath='local'",
            "[sources.x]\npath='local'\nrename={a='..'}", "[sources.x]\npath='local'\nrename={missing='renamed'}",
            "[sources.x]\npath='local'\nskills=['.']", "[sources.x]\npath='local'\nskills=['a']",
            "[sources.x]\nrepo='-invalid'", "[sources.x]\nrepo='upstream'\nref=''",
        ]
        for source in sources:
            with self.subTest(source=source):
                def scenario():
                    self.write('local/a/SKILL.md', 'alpha\n')
                    self.manifest(source)
                    self.run_cli('sync', success=None)
                self.compare(scenario)

    def test_corrupt_state_and_lock(self):
        for field in ['version', 'owner', 'targets', 'links', 'skills']:
            for value in ['missing', None]:
                with self.subTest(field=field, value=value):
                    def scenario():
                        self.write('local/a/SKILL.md', 'a\n')
                        self.manifest("[sources.x]\npath='local'")
                        self.run_cli('sync')
                        state = json.loads((self.root / 'store/state.json').read_text())
                        if value == 'missing':
                            del state[field]
                        else:
                            state[field] = value
                        self.write('store/state.json', json.dumps(state))
                        self.run_cli('sync', success=None)
                    self.compare(scenario)
        for field in ['version', 'owner', 'skills']:
            with self.subTest(duplicate_state_field=field):
                def scenario():
                    self.write('local/a/SKILL.md', 'a\n')
                    self.manifest("[sources.x]\npath='local'")
                    self.run_cli('sync')
                    raw = (self.root / 'store/state.json').read_text()
                    state = json.loads(raw)
                    self.write('store/state.json', '{' + json.dumps(field) + ':' + json.dumps(state[field]) + ',' + raw[1:])
                    self.run_cli('sync', success=False)
                self.compare(scenario)
        for field in ['kind', 'upstream', 'revision', 'patches', 'tree_sha256']:
            with self.subTest(lock_field=field):
                def scenario():
                    self.write('local/a/SKILL.md', 'a\n')
                    self.manifest("[sources.x]\npath='local'")
                    self.run_cli('sync')
                    lines = (self.root / 'skills.lock').read_text().splitlines(keepends=True)
                    self.write('skills.lock', ''.join(line for line in lines if not line.startswith(field + ' =')))
                    self.run_cli('sync', success=None)
                self.compare(scenario)

    def test_glob_corpus(self):
        for pattern in ['**', '**/', '**alpha', 'p/**a', 'p/**/alpha', '{p,{q,r}}/**', '{,p}/**', '{**/alpha,p/**}', 'p/[!x]*/alpha', 'p/[]a]/alpha', 'p/[-a]/alpha', 'p/[a-]/alpha', '**/??', '**/é', '**/a?']:
            with self.subTest(pattern=pattern):
                def scenario():
                    for path in ['p/a/alpha', 'p/q/beta', 'p/alpha', 'q/a/alpha', 'é', 'aé']:
                        self.write('local/' + path + '/SKILL.md', path + '\n')
                    # Each source uses one path to avoid name collisions and unused rename keys.
                    for i, path in enumerate(['p/a/alpha', 'p/q/beta', 'p/alpha', 'q/a/alpha', 'é', 'aé']):
                        shutil.rmtree(self.root / 'local')
                        self.write('local/' + path + '/SKILL.md', path + '\n')
                        self.manifest('[sources.x]\npath="local"\nskills=[' + json.dumps(pattern, ensure_ascii=False) + ']\nrename={' + json.dumps(path, ensure_ascii=False) + '="selected"}')
                        self.run_cli('sync', success=None)
                self.compare(scenario)

    def test_generation_corpus(self):
        for before, after in [
            ('# T\n\n## A\n\n### B\nold text\n', '# T\n\n## A\n\n### B\nnew text\n'),
            ('Anchor\n------\n\nBody.\n\n## Next\nOther.\n', 'Anchor\n------\n\nBody.\n\n## Extra\n\nraw body\n\n## Next\nOther.\n'),
            ('text old text\n', 'text "new" text\n'),
            ('text old text\n', "text 'new' text\n"),
            ('old\n', 'new'),
            ('## A\nx\n\nx\n', '## A\ny\n\nx\n'),
            ('## A\nold\n', '## B\nold\n'),
            ('plain text\n', 'plain  text\n'),
            ('old\n', '*new*\n'),
            ('## A\ntext\n', '## A\ntext\n\n## B\n\nquote "double" and \'single\'\n'),
            ('## A\ntext\r\n', '## A\ntext\r\n\n## B\n\nnew\n'),
        ]:
            with self.subTest(before=before, after=after):
                def scenario():
                    self.write('local/a/SKILL.md', before)
                    self.write('edited.md', after)
                    self.manifest("[sources.x]\npath='local'")
                    self.run_cli('sync')
                    self.run_cli('patch', 'generate', 'x', 'a/SKILL.md', 'edited.md', '--output', 'generated.skillpatch.toml', success=None)
                self.compare(scenario)

    def test_patch_schema_and_limits(self):
        valid = self.replacement('target')
        operations = [
            valid.replace('replace_text', 'remove_sentence'),
            valid.replace('replace_text', 'replace_regex'),
            valid.replace('min = 1', 'min = 2'),
            valid.replace('max = 1', 'max = -1'),
            valid.replace('max = 1', 'max = 1, unknown = 0'),
            valid + '\nunknown = true',
            valid.replace('section = []\n', ''),
            valid.replace('expect = {min = 1, max = 1}', 'expect = {min = 1}'),
            valid.replace('find = "target"', 'find = " "'),
            '\n'.join([valid] * 257),
            self.replacement('target', 'x' * (1024 * 512), minimum=3),
        ]
        for i, operation in enumerate(operations):
            with self.subTest(case=i):
                def scenario():
                    self.write('local/a/SKILL.md', 'target target target\n')
                    self.patch(operation)
                    self.run_cli('sync', success=False)
                self.compare(scenario)
        for path in ['../a.md', '/a.md', 'a/./SKILL.md', 'a//SKILL.md', '.git/a.md', 'a.txt']:
            with self.subTest(path=path):
                def scenario():
                    self.write('local/a/SKILL.md', 'target\n')
                    self.patch(valid, file=path)
                    self.run_cli('sync', success=False)
                self.compare(scenario)
        def large_document():
            self.write('local/a/SKILL.md', 'target ' * 150000)
            self.patch(valid)
            self.run_cli('sync', success=False)
        self.compare(large_document)

    def test_archive_attributes_and_submodules(self):
        def scenario():
            self.repository()
            self.write('upstream/ignored/SKILL.md', 'ignored\n')
            self.write('upstream/.gitattributes', 'ignored export-ignore\nsubstituted export-subst\n')
            self.write('upstream/substituted', '$Format:%H$\n')
            self.write('upstream/run.sh', '#!/bin/sh\necho helper\n')
            (self.root / 'upstream/run.sh').chmod(0o755)
            (self.root / 'upstream/plugins/tools/skills/alpha/helper').symlink_to('../../../../run.sh')
            self.git('add', '.')
            self.git('commit', '-m', 'attributes and symlink')
            self.manifest("[sources.x]\nrepo='upstream'")
            self.run_cli('sync')
            commit = self.git('rev-parse', 'HEAD')
            self.git('update-index', '--add', '--cacheinfo', '160000,' + commit + ',module')
            self.git('commit', '-m', 'gitlink')
            self.run_cli('update', success=False)
            self.run_cli('sync', '--locked', '--offline')
        self.compare(scenario)

    def test_git_and_markdown_patch_order(self):
        def scenario():
            self.repository()
            self.write('one.patch', 'diff --git a/plugins/tools/skills/alpha/SKILL.md b/plugins/tools/skills/alpha/SKILL.md\n--- a/plugins/tools/skills/alpha/SKILL.md\n+++ b/plugins/tools/skills/alpha/SKILL.md\n@@ -1 +1 @@\n-alpha v1\n+alpha patched\n')
            self.write('two.skillpatch.toml', 'version = 1\nfile = "plugins/tools/skills/alpha/SKILL.md"\n' + self.replacement('patched', 'chosen'))
            self.manifest("[sources.x]\nrepo = 'upstream'\npatches = ['one.patch', 'two.skillpatch.toml']")
            self.run_cli('sync', '--dry-run')
            self.run_cli('sync')
            self.run_cli('sync', '--locked', '--offline')
            self.write('one.patch', 'broken')
            self.run_cli('sync', '--locked', success=False)
            self.run_cli('sync', success=False)
        self.compare(scenario)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--rust', type=Path, required=True)
    parser.add_argument('--go', type=Path, required=True)
    args, remaining = parser.parse_known_args()
    Parity.rust = args.rust.resolve()
    Parity.go = args.go.resolve()
    unittest.main(argv=['parity', *remaining])
