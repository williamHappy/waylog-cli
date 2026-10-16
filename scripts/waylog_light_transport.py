"""Opt-in v2 transport. Reuses legacy local Git/locks/timeouts/backoff, no audit.

This module never installs tasks. Tests inject mock upload or temporary bare repos.
"""
import contextlib
from pathlib import Path
import tempfile
import time
from waylog_daily_status import (Reporter, atomic_json, local_document, merge_receipt,
                                read_json, set_enabled)
from waylog_schedule import Schedule


def service_class(legacy):
    class LightService(legacy.Service):
        def __init__(self, args):
            super().__init__(args)
            if not self.direct:
                raise ValueError('daily status requires an existing direct Git repository')
            self.schedule = Schedule(args.sync_schedule)
            self.local_path = self.state / 'daily-status.json'
            self.report_path = self.state / 'daily-report-state.json'

        def success_deadline(self, attempt_epoch):
            if self.schedule.mode == 'cron':
                return self.schedule.next_after(time.time())
            return super().success_deadline(attempt_epoch)

        @contextlib.contextmanager
        def locked(self):
            with self.lock('sync'), self.archive_access():
                if (self.state / 'BLOCKED').exists():
                    raise legacy.ServiceError('Resolve BLOCKED state before syncing')
                yield

        def read_local(self):
            return local_document(self.local_path, self.remote_path.name)

        def has_data_changes(self):
            self.check_repo(allow_archive=True)
            prefix = self.remote_path.as_posix()
            names = self.git('diff', '--name-only', '-z', 'HEAD', '--', prefix)[1].split('\0')
            names += self.git('ls-files', '--others', '--exclude-standard', '-z', '--', prefix)[1].split('\0')
            return any(n and n != prefix + '/indexes/coverage.json' for n in names)

        def allowed(self):
            return self.network_allowed()

        def finish(self, success):
            self.finish_network(success)
            updates = {'result': 'success' if success else 'failed', 'sync_mode': 'daily-status'}
            if not success:
                updates['upload_result'] = 'unknown'
            self.record(**updates)

        def pull(self):
            self.check_repo(allow_archive=True)
            self.verify_direct_pending()
            self.git('fetch', 'origin', self.args.branch)
            self.git('merge', '--ff-only', 'origin/' + self.args.branch)
            self.record(last_pull=legacy.stamp())

        def freeze(self, pending, save):
            head = self.git('rev-parse', 'HEAD')[1]
            marker = 'Waylog-Daily-Report: ' + pending['reportId']
            # Crash after commit/before queue save: recover that exact commit.
            if 'baseHead' in pending and head != pending['baseHead']:
                message = self.git('show', '-s', '--format=%B', head)[1].splitlines()
                parents = self.git('show', '-s', '--format=%P', head)[1].split()
                if marker in message and parents == [pending['baseHead']]:
                    pending['commit'] = head
                    save()
                    return
                raise legacy.ServiceError('HEAD changed during pending report; inspect before retry')
            self.check_repo(allow_archive=True)
            self.verify_direct_pending()
            self.pull()
            pending['baseHead'] = self.git('rev-parse', 'HEAD')[1]
            save()
            target = self.safe_destination(Path('indexes/coverage.json'))
            existing = read_json(target, None)
            payload = merge_receipt(existing, pending['receipt'], time.time())
            atomic_json(target, payload)
            prefix = self.remote_path.as_posix()
            paths = [prefix + '/indexes/coverage.json']
            if pending['includeData']:
                paths = [prefix + '/' + item for item in legacy.ARCHIVE_ITEMS
                         if (self.source / item).exists()]
            # Guard against link-based staging outside the owned archive.
            for item in self.source.rglob('*') if pending['includeData'] else [target]:
                self.safe_destination(item.relative_to(self.source))
            with tempfile.NamedTemporaryFile(dir=self.state, delete=False) as stream:
                spec = Path(stream.name)
                stream.write(b''.join(p.encode('utf-8') + b'\0' for p in paths))
            try:
                self.git('--literal-pathspecs', 'add', f'--pathspec-from-file={spec}', '--pathspec-file-nul')
            finally:
                spec.unlink(missing_ok=True)
            code = self.git('diff', '--cached', '--quiet', check=False)[0]
            if code not in (0, 1):
                raise legacy.ServiceError('Cannot inspect staged report')
            if code:
                self.git('-c', 'user.name=' + self.args.git_name, '-c', 'user.email=' + self.args.git_email,
                         'commit', '-m', 'chore(archive): collection status\n\nWaylog-Service-Archive: '
                         + prefix + '\n' + marker)
            pending['commit'] = self.git('rev-parse', 'HEAD')[1]
            save()  # Frozen durably before any LFS or Git upload.

        def upload(self, commit):
            self.check_repo(allow_archive=True)
            self.record(upload_result='in-flight')
            if self.git('rev-parse', 'HEAD')[1] != commit:
                raise legacy.ServiceError('Frozen report HEAD changed; no rebase or force push')
            self.verify_direct_pending()
            # Begin reservation is performed by inherited Git wrapper. At most
            # one LFS transfer attempt per object; inherited timeout is retained.
            self.git('lfs', 'push', 'origin', commit)
            self._lfs_ready_for_push = True
            self.git('push', 'origin', commit + ':refs/heads/' + self.args.branch)
            self.record(last_push=legacy.stamp(), last_uploaded_commit=commit, upload_result='success')

        def dispatch(self):
            return Reporter(self.report_path, self.remote_path.name, self.args.sync_schedule,
                            self.args.daily_status['checkpoint'], self, time.time).poll()

        def control(self, enabled):
            with self.archive_access():
                set_enabled(self.local_path, self.remote_path.name, enabled)
    return LightService
