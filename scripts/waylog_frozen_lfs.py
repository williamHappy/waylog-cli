"""Local-only, journaled LFS conversion of an unpublished frozen report.

Use Git plumbing so later collector writes and unrelated references are untouched.
No fetch, push, checkout, reset, or network operation is performed here.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time


class FrozenLFS:
    def __init__(self, service, pending, save):
        self.s, self.p, self.save = service, pending, save

    def fail(self, message):
        raise RuntimeError('Frozen LFS recovery: ' + message)

    def git(self, *args, data=None, env=None, stdout=subprocess.PIPE):
        process_env = dict(self.s.env, GIT_LFS_SKIP_SMUDGE='1', GIT_NO_LAZY_FETCH='1')
        if env:
            process_env.update(env)
        timeout = self.s.args.timeout
        deadline = getattr(self.s, '_push_deadline', None)
        if deadline is not None:
            timeout = min(timeout, deadline - time.monotonic())
        if timeout <= 0:
            self.fail('time budget exhausted; original batch retained')
        options = {'creationflags': subprocess.CREATE_NO_WINDOW} if os.name == 'nt' else {}
        result = subprocess.run([self.s.git_path, '-C', str(self.s.repo),
            '-c', 'core.hooksPath=' + str(self.s.state / 'lfs-backups' / 'no-hooks'), *args],
            input=data, stdout=stdout, stderr=subprocess.PIPE, env=process_env,
            timeout=timeout, **options)
        if result.returncode:
            self.fail(result.stderr.decode('utf-8', errors='replace').strip())
        return result.stdout if stdout == subprocess.PIPE else b''

    def oid(self, *args):
        return self.git(*args).decode().strip()

    def attributes(self, commit):
        entries = self.git('ls-tree', '-z', commit, '--', '.gitattributes')
        if not entries:
            return None
        info, _ = entries.rstrip(b'\0').split(b'\t', 1)
        mode, kind, oid = info.split()
        if mode != b'100644' or kind != b'blob':
            self.fail('unsupported .gitattributes type')
        return self.git('cat-file', 'blob', oid.decode())

    def current_attributes(self):
        path = self.s.repo / '.gitattributes'
        if path.is_symlink():
            self.fail('symlink .gitattributes')
        return path.read_bytes() if path.exists() else None

    def published(self):
        return self.oid('rev-parse', 'refs/remotes/origin/' + self.s.args.branch)

    def branch(self):
        branch = 'refs/heads/' + self.s.args.branch
        if self.oid('symbolic-ref', 'HEAD') != branch:
            self.fail('unexpected active branch')
        return branch

    def prepare(self, output):
        paths = sorted(set(re.findall(
            r'(?m)^remote:\s+error:\s+File (.+?) is [0-9.]+ (?:MB|MiB); this exceeds', output)))
        prefix = self.s.remote_path.as_posix() + '/'
        if not paths or any(not p.startswith(prefix) or '..' in Path(p).parts
                or any(c in p for c in ',*?[]\\\r\n"') or any(ord(c) < 32 for c in p) for p in paths):
            self.fail('missing or unsafe rejected file paths; inspect manually')
        self.s.check_repo(allow_archive=True)
        self.s.verify_direct_pending()
        branch = self.branch()
        old = self.oid('rev-parse', 'HEAD')
        frozen = self.p['commit']
        candidate = self.p.get('uploadCommit', frozen)
        if old != candidate or self.p.get('integration'):
            self.fail('batch HEAD or merge reservation changed')
        self.s.check_frozen_candidate(frozen, candidate)
        for path in paths:
            if not self.git('ls-tree', '-z', candidate, '--', path):
                self.fail('reported file is not in the frozen batch')
        published = self.published()
        self.git('merge-base', '--is-ancestor', published, candidate)
        commits = self.oid('rev-list', '--reverse', '--topo-order', published + '..' + candidate).splitlines()
        if frozen not in commits:
            self.fail('frozen batch already published; refusing to replace it')
        # A cached published ref or a tag may name an otherwise unpublished commit.
        protected = self.git('for-each-ref', '--format=%(refname)', 'refs/remotes/', 'refs/tags/').decode().splitlines()
        for ref in protected:
            reachable = set(self.oid('rev-list', ref).splitlines())
            if reachable.intersection(commits):
                self.fail('pending history is reachable from protected ref ' + ref)
        if self.oid('write-tree') != self.oid('rev-parse', candidate + '^{tree}'):
            self.fail('staged changes must be inspected before recovery')
        old_attrs = self.attributes(candidate)
        if self.current_attributes() != old_attrs:
            self.fail('local .gitattributes changed; leaving it untouched')
        backups = self.s.state / 'lfs-backups'
        backups.mkdir(parents=True, exist_ok=True)
        folder = Path(tempfile.mkdtemp(prefix='frozen-', dir=backups))
        bundle = folder / 'original.bundle'
        self.git('bundle', 'create', str(bundle), branch, '^' + published)
        self.git('bundle', 'verify', str(bundle))
        (folder / 'original.json').write_text(json.dumps({'pending': self.p,
            'published': published, 'branch': branch, 'head': old}, ensure_ascii=False, indent=2))
        media = next((line[len('LocalMediaDir='):] for line in
            self.git('lfs', 'env').decode().splitlines() if line.startswith('LocalMediaDir=')), None)
        if not media or not Path(media).is_absolute():
            self.fail('cannot locate local LFS object storage')
        mapping, converted = {}, {}
        with tempfile.TemporaryDirectory(prefix='frozen-index-', dir=self.s.state) as temporary:
            index = str(Path(temporary) / 'index')
            for commit in commits:
                raw = self.git('cat-file', 'commit', commit)
                header, message = raw.split(b'\n\n', 1)
                if any(line.split(b' ', 1)[0] not in (b'tree', b'parent', b'author', b'committer', b'encoding')
                       for line in header.splitlines()):
                    self.fail('signed or unsupported commit headers; inspect manually')
                entries = self.git('ls-tree', '-r', '-z', commit)
                replacements = []
                for entry in entries.split(b'\0'):
                    if not entry:
                        continue
                    info, path = entry.split(b'\t', 1)
                    name = path.decode('utf-8')
                    if name not in paths:
                        continue
                    mode, kind, blob = info.split()
                    if kind != b'blob' or mode not in (b'100644', b'100755'):
                        self.fail('unsupported rejected file type')
                    key = blob.decode()
                    if key not in converted and int(self.oid('cat-file', '-s', key)) < 1024:
                        small = self.git('cat-file', 'blob', key)
                        if re.fullmatch(rb'version https://git-lfs.github.com/spec/v1\noid sha256:[0-9a-f]{64}\nsize [0-9]+\n', small):
                            converted[key] = blob
                    if key not in converted:
                        with tempfile.TemporaryFile(dir=folder) as stream:
                            self.git('cat-file', 'blob', key, stdout=stream)
                            stream.seek(0)
                            digest, size = hashlib.sha256(), 0
                            while True:
                                chunk = stream.read(1024 * 1024)
                                if not chunk:
                                    break
                                digest.update(chunk); size += len(chunk)
                            sha = digest.hexdigest()
                            target = Path(media) / sha[:2] / sha[2:4] / sha
                            target.parent.mkdir(parents=True, exist_ok=True)
                            if target.is_symlink():
                                self.fail('symlink LFS object cache entry')
                            if target.exists():
                                cached = hashlib.sha256()
                                with target.open('rb') as existing:
                                    while True:
                                        chunk = existing.read(1024 * 1024)
                                        if not chunk:
                                            break
                                        cached.update(chunk)
                                if target.stat().st_size != size or cached.hexdigest() != sha:
                                    self.fail('existing LFS cache content is invalid')
                            else:
                                stream.seek(0)
                                with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as out:
                                    temp = Path(out.name)
                                    while True:
                                        chunk = stream.read(1024 * 1024)
                                        if not chunk:
                                            break
                                        out.write(chunk)
                                os.replace(temp, target)
                        pointer = f'version https://git-lfs.github.com/spec/v1\noid sha256:{sha}\nsize {size}\n'.encode()
                        converted[key] = self.git('hash-object', '-w', '--stdin', data=pointer).strip()
                    replacements.append(mode + b' ' + converted[key] + b'\t' + path + b'\0')
                attrs = self.attributes(commit)
                if replacements:
                    content = attrs or b''
                    if content and not content.endswith(b'\n'):
                        content += b'\n'
                    for name in paths:
                        rule = (json.dumps(name, ensure_ascii=False) + ' filter=lfs diff=lfs merge=lfs -text\n').encode()
                        if rule not in content.splitlines(keepends=True):
                            content += rule
                    attr_oid = self.git('hash-object', '-w', '--stdin', data=content).strip()
                    replacements.append(b'100644 ' + attr_oid + b'\t.gitattributes\0')
                index_env = {'GIT_INDEX_FILE': index}
                self.git('read-tree', commit, env=index_env)
                if replacements:
                    self.git('update-index', '-z', '--index-info', data=b''.join(replacements), env=index_env)
                tree = self.oid_with_env(index_env, 'write-tree')
                new_header = []
                for line in header.splitlines():
                    key, value = line.split(b' ', 1)
                    if key == b'tree': value = tree.encode()
                    if key == b'parent': value = mapping.get(value.decode(), value.decode()).encode()
                    new_header.append(key + b' ' + value)
                mapping[commit] = self.git('hash-object', '-t', 'commit', '-w', '--stdin',
                    data=b'\n'.join(new_header) + b'\n\n' + message).decode().strip()
        if mapping[candidate] == candidate or mapping[frozen] == frozen:
            self.fail('reported paths did not replace the frozen batch')
        new_attrs = self.attributes(mapping[candidate])
        journal = {'phase': 'prepared', 'branch': branch, 'published': published,
            'oldCommit': frozen, 'oldCandidate': candidate, 'newCommit': mapping[frozen],
            'newCandidate': mapping[candidate], 'newBaseHead': mapping.get(self.p.get('baseHead'), self.p.get('baseHead')),
            'oldAttributes': None if old_attrs is None else base64.b64encode(old_attrs).decode(),
            'newAttributes': base64.b64encode(new_attrs).decode(), 'backup': str(bundle), 'files': paths, 'mapping': mapping}
        (folder / 'mapping.json').write_text(json.dumps(mapping, indent=2))
        self.p['lfsRecovery'] = journal
        self.save()  # Durable intent BEFORE replacing the branch, index, or queue identity.
        self.resume()

    def oid_with_env(self, env, *args):
        return self.git(*args, env=env).decode().strip()

    def verify_mapping(self, journal):
        """Recheck prepared objects after a crash before trusting their new IDs."""
        mapping = journal['mapping']
        expected = set(self.oid('rev-list', journal['published'] + '..' + journal['oldCandidate']).splitlines())
        if set(mapping) != expected or mapping.get(journal['oldCommit']) != journal['newCommit'] or mapping.get(journal['oldCandidate']) != journal['newCandidate']:
            self.fail('recovery mapping differs from unpublished batch')
        digests = {}
        for old, new in mapping.items():
            before, after = [], []
            for commit, target in ((old, before), (new, after)):
                for entry in self.git('ls-tree', '-r', '-z', commit).split(b'\0'):
                    if entry:
                        info, name = entry.split(b'\t', 1)
                        mode, kind, oid = info.split()
                        target.append((name, (mode, kind, oid)))
            before, after = dict(before), dict(after)
            names = set(before) | set(after)
            for name in names:
                if name == b'.gitattributes':
                    continue
                a, b = before.get(name), after.get(name)
                if a == b:
                    continue
                if name.decode('utf-8') not in journal['files'] or not a or not b or a[:2] != b[:2]:
                    self.fail('replacement changed files outside the rejected payload')
                pointer = self.git('cat-file', 'blob', b[2].decode())
                match = re.fullmatch(rb'version https://git-lfs.github.com/spec/v1\noid sha256:([0-9a-f]{64})\nsize ([0-9]+)\n', pointer)
                if not match:
                    self.fail('replacement is not a canonical LFS pointer')
                key = a[2].decode()
                if key not in digests:
                    with tempfile.TemporaryFile(dir=self.s.state) as stream:
                        self.git('cat-file', 'blob', key, stdout=stream)
                        stream.seek(0); digest, size = hashlib.sha256(), 0
                        while True:
                            chunk = stream.read(1024 * 1024)
                            if not chunk:
                                break
                            digest.update(chunk); size += len(chunk)
                    digests[key] = (digest.hexdigest(), size)
                if (match[1].decode(), int(match[2])) != digests[key]:
                    self.fail('LFS payload differs from original frozen bytes')
            attrs = self.attributes(old) or b''
            if any(p.encode() in before for p in journal['files']):
                if attrs and not attrs.endswith(b'\n'):
                    attrs += b'\n'
                for path in journal['files']:
                    rule = (json.dumps(path, ensure_ascii=False) + ' filter=lfs diff=lfs merge=lfs -text\n').encode()
                    if rule not in attrs.splitlines(keepends=True):
                        attrs += rule
            if (self.attributes(new) or b'') != attrs:
                self.fail('replacement changed unrelated LFS rules')
            header, message = self.git('cat-file', 'commit', old).split(b'\n\n', 1)
            lines = []
            for line in header.splitlines():
                key, value = line.split(b' ', 1)
                if key == b'tree':
                    value = self.oid('rev-parse', new + '^{tree}').encode()
                if key == b'parent':
                    value = mapping.get(value.decode(), value.decode()).encode()
                lines.append(key + b' ' + value)
            expected_id = self.git('hash-object', '-t', 'commit', '--stdin',
                data=b'\n'.join(lines) + b'\n\n' + message).decode().strip()
            if expected_id != new:
                self.fail('replacement changed commit metadata or published parents')

    def resume(self):
        j = self.p.get('lfsRecovery')
        if not j or j.get('phase') == 'complete':
            return
        if j.get('phase') not in ('prepared', 'applied'):
            self.fail('invalid recovery journal phase')
        for key in ('published', 'oldCommit', 'oldCandidate', 'newCommit', 'newCandidate'):
            if not re.fullmatch(r'[0-9a-f]{40,64}', j.get(key, '')):
                self.fail('invalid recovery commit identity')
        if self.p.get('commit') not in (j['oldCommit'], j['newCommit']):
            self.fail('pending batch changed during recovery')
        self.git('merge-base', '--is-ancestor', j['published'], j['newCandidate'])
        self.verify_mapping(j)
        if self.branch() != j['branch'] or self.published() != j['published']:
            self.fail('branch or cached published boundary changed during recovery')
        head = self.oid('rev-parse', 'HEAD')
        candidates = (j['oldCandidate'], j['newCandidate']) if j['phase'] == 'prepared' else (j['newCandidate'],)
        if head not in candidates:
            self.fail('HEAD changed during recovery')
        trees = {self.oid('rev-parse', c + '^{tree}') for c in candidates}
        if self.oid('write-tree') not in trees:
            self.fail('index changed during recovery')
        old_attrs = None if j['oldAttributes'] is None else base64.b64decode(j['oldAttributes'])
        new_attrs = base64.b64decode(j['newAttributes'])
        if new_attrs != self.attributes(j['newCandidate']):
            self.fail('journal attributes differ from replacement commit')
        allowed_attrs = (old_attrs, new_attrs) if j['phase'] == 'prepared' else (new_attrs,)
        if self.current_attributes() not in allowed_attrs:
            self.fail('.gitattributes changed during recovery')
        if j['phase'] == 'prepared':
            if head == j['oldCandidate']:
                self.git('update-ref', j['branch'], j['newCandidate'], j['oldCandidate'])
            self.git('read-tree', j['newCandidate'])  # Index only: preserve all later collector writes.
            path = self.s.repo / '.gitattributes'
            with tempfile.NamedTemporaryFile(dir=self.s.repo, delete=False) as stream:
                temp = Path(stream.name); stream.write(new_attrs)
            os.replace(temp, path)
            self.p['commit'] = j['newCommit']
            self.p['uploadCommit'] = j['newCandidate']
            if j['newBaseHead'] is not None:
                self.p['baseHead'] = j['newBaseHead']
            self.p.pop('integration', None)
            j['phase'] = 'applied'
            self.save()
        # Do not clear a retry-limit pause or an unrelated manual pause.
        network = self.s.network_state()
        if network.get('pause_reason', '').startswith('GitHub GH001 rejected large files'):
            network['paused'] = network['consecutive_failures'] >= self.s.retry_max_failures
            if not network['paused']:
                network.pop('pause_reason', None)
            if not self.s._network_active:
                network['in_flight'] = False
            self.s.save_network_state(network)
        j['phase'] = 'complete'
        self.save()
        self.s.record(lfs_recovery='ready-for-next-cycle', lfs_backup=j['backup'],
            lfs_old_head=j['oldCandidate'], lfs_new_head=j['newCandidate'],
            frozen_report_commit=j['newCommit'], frozen_report_preserved=False)
