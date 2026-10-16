"""Opt-in v2 transport. Reuses legacy local Git/locks/timeouts/backoff, no audit.

This module never installs tasks. Tests inject mock upload or temporary bare repos.
"""
import contextlib
from pathlib import Path
import tempfile
import time
from waylog_daily_status import (Reporter, atomic_json, local_document, merge_receipt,
                                read_json, set_enabled, status_warning, StatusUnavailable, validate)
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

        def skip_status(self, pending, save, error):
            status_warning(self, error)
            pending.update(receipt=None, checkpointDays=[], statusSkipped=True)
            save()

        def optional_payload(self, target, pending, save):
            if pending.get('receipt') is None:
                return None
            try:
                validate(pending['receipt'], self.remote_path.name, time.time())
                existing = read_json(target, None)
                return merge_receipt(existing, pending['receipt'], time.time())
            except (ValueError, TypeError, KeyError, AttributeError, OSError) as error:
                self.skip_status(pending, save, error)
                return None

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
            if pending['includeData']:
                # Archive identity, index syntax and referenced files remain
                # mandatory even when optional operational status is skipped.
                self.validate_snapshot(self.source)
            target = self.safe_destination(Path('indexes/coverage.json'))
            payload = self.optional_payload(target, pending, save)
            if payload is None and not pending['includeData']:
                raise StatusUnavailable('No valid status or archive data to upload')
            self.pull()
            pending['baseHead'] = self.git('rev-parse', 'HEAD')[1]
            save()
            # Fetch may have updated the existing receipt. Merge it again while
            # holding the archive lock, without changing the frozen observation.
            payload = self.optional_payload(target, pending, save)
            if payload is None and not pending['includeData']:
                raise StatusUnavailable('No valid status or archive data to upload')
            if payload is not None:
                try:
                    atomic_json(target, payload)
                except OSError as error:
                    self.skip_status(pending, save, error)
                    if not pending['includeData']:
                        raise StatusUnavailable('Cannot write optional status') from error
                    payload = None
            prefix = self.remote_path.as_posix()
            paths = [prefix + '/indexes/coverage.json'] if payload is not None else []
            # Select files explicitly so an invalid coverage.json is never
            # accidentally staged by adding the whole indexes directory.
            for item in self.source.rglob('*') if pending['includeData'] else [target]:
                self.safe_destination(item.relative_to(self.source))
                if (pending['includeData'] and item.is_file() and item != target
                        and item.relative_to(self.source).parts[0] in legacy.ARCHIVE_ITEMS):
                    paths.append(prefix + '/' + item.relative_to(self.source).as_posix())
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

        def recover_large_files(self, output):
            # A frozen batch must retain its original commit, even for a real
            # GH001 rejection. Legacy automatic LFS migration rewrites history.
            self.record(lfs_recovery='needs-manual-review', frozen_report_preserved=True)

        def frozen_tree(self, commit):
            return self.git('rev-parse', commit + ':' + self.remote_path.as_posix())[1]

        def check_frozen_candidate(self, frozen, candidate):
            if (self.git('merge-base', '--is-ancestor', frozen, candidate, check=False)[0]
                    or self.frozen_tree(frozen) != self.frozen_tree(candidate)):
                raise legacy.ServiceError('Frozen report ancestry or archive tree changed; inspect before retry')

        def recover_integration(self, frozen, pending, save):
            candidate = pending.get('uploadCommit', frozen)
            head = self.git('rev-parse', 'HEAD')[1]
            intent = pending.get('integration')
            if head != candidate:
                # Only recover the exact two-parent merge reserved before a
                # crash. An unrelated HEAD change must never be acknowledged.
                parents = self.git('show', '-s', '--format=%P', head)[1].split()
                reserved_merge = intent and parents == [candidate, intent['remote']]
                reserved_fast_forward = (intent and head == intent['remote'] and
                    self.git('merge-base', '--is-ancestor', candidate, head, check=False)[0] == 0)
                if not (reserved_merge or reserved_fast_forward):
                    raise legacy.ServiceError('Frozen report HEAD changed unexpectedly; inspect before retry')
                self.check_frozen_candidate(frozen, head)
                self.verify_direct_pending()
                pending['uploadCommit'] = head
            self.check_frozen_candidate(frozen, pending.get('uploadCommit', frozen))
            pending.pop('integration', None)
            save()

        def merge_remote_report(self, frozen, pending, save):
            self.git('fetch', 'origin', self.args.branch)
            remote = self.git('rev-parse', 'origin/' + self.args.branch)[1]
            candidate = pending.get('uploadCommit', frozen)
            # Another device must not replace any part of this frozen payload,
            # even if Git could resolve the change without a textual conflict.
            base = self.git('merge-base', candidate, remote)[1]
            published = self.git('merge-base', '--is-ancestor', candidate, remote, check=False)[0] == 0
            if published:
                changed = self.frozen_tree(remote) != self.frozen_tree(frozen)
            else:
                changed = self.git('diff', '--quiet', base, remote, '--',
                                   self.remote_path.as_posix(), check=False)[0] != 0
            if changed:
                (self.state / 'BLOCKED').write_text('Remote modified this device archive; inspect frozen report before syncing.\n')
                raise legacy.ServiceError('Remote modified frozen report archive; sync blocked')
            pending['integration'] = {'remote': remote}
            save()  # Reservation survives a crash after merge, before queue save.
            code, output = self.git('-c', 'user.name=' + self.args.git_name,
                                    '-c', 'user.email=' + self.args.git_email,
                                    'merge', '--no-edit', remote, check=False)
            if code:
                self.git('merge', '--abort', check=False)
                (self.state / 'BLOCKED').write_text('Frozen report merge failed; inspect manually.\n' + output + '\n')
                raise legacy.ServiceError('Frozen report merge failed; sync blocked')
            self.recover_integration(frozen, pending, save)

        def upload(self, commit, pending, save):
            self.check_repo(allow_archive=True)
            self.verify_direct_pending()
            self.recover_integration(commit, pending, save)
            self.record(upload_result='in-flight')
            uploaded = self.push_with_retry(
                reconcile=lambda: self.merge_remote_report(commit, pending, save),
                revision=lambda: pending.get('uploadCommit', commit))
            if not pending.get('statusSkipped'):
                self.record(status_warning=None)
            self.record(last_push=legacy.stamp(), last_success=legacy.stamp(),
                        last_error=None, last_uploaded_commit=uploaded,
                        last_uploaded_archive_tree=self.frozen_tree(uploaded),
                        frozen_report_commit=commit, upload_result='success')
            return uploaded

        def dispatch(self):
            try:
                return Reporter(self.report_path, self.remote_path.name, self.args.sync_schedule,
                                self.args.daily_status['checkpoint'], self, time.time).poll()
            except Exception as error:
                self.record(last_error=str(error))
                raise

        def control(self, enabled):
            with self.archive_access():
                set_enabled(self.local_path, self.remote_path.name, enabled)
    return LightService
