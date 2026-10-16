#!/usr/bin/env python3
"""Offline races: all Git remotes are disposable local bare repositories."""
import copy
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest import mock


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


base = load('race_base', 'test-waylog-service.py')
daily = load('race_daily', 'test-waylog-daily-status.py')
git = base.git
WARNING = ('remote: warning: File example.raw.jsonl is 88.34 MB\n'
           'remote: warning: GH001: Large files detected. You may want to try Git Large File Storage.\n')
RACE = "!\tHEAD:refs/heads/main\t[remote rejected] (cannot lock ref 'refs/heads/main': is at aaaa but expected bbbb)\n"


class ClassificationTests(unittest.TestCase):
    def test_warning_plus_race_is_race(self):
        self.assertEqual(base.module.Service.push_failure_kind(WARNING + RACE), 'remote-advanced')
    def test_warning_plus_network_failure_is_not_size_rejection(self):
        self.assertEqual(base.module.Service.push_failure_kind(WARNING + 'fatal: connection reset'), 'other')
    def test_real_size_error_takes_priority(self):
        self.assertEqual(base.module.Service.push_failure_kind(WARNING + RACE + 'remote: error: GH001: Large files detected.\n'), 'large-file')
    def test_only_explicit_branch_races_are_retryable(self):
        for reason in ('fetch first', 'non-fast-forward'):
            self.assertEqual(base.module.Service.push_failure_kind('!\tHEAD:refs/heads/main\t[rejected] (' + reason + ')'), 'remote-advanced')
        for text in ('fatal: cannot lock ref refs/heads/main', 'remote: error: permission denied', '[remote rejected] (pre-receive hook declined)'):
            self.assertEqual(base.module.Service.push_failure_kind(text), 'other')


class LegacyRaceTests(unittest.TestCase):
    setUp = base.DirectSyncTests.setUp
    def advance(self, number, own=False):
        path = self.other / ('ai/raw/wk-me/sessions/session.md' if own else 'ai/raw/wk-jzx/item.md')
        git(self.other, 'pull', '--ff-only'); path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('remote ' + str(number)); git(self.other, 'add', '.')
        git(self.other, 'commit', '-m', 'other device'); git(self.other, 'push')
    def install_race(self, every=False, own=False, warning=False):
        original = self.service.git; self.pushes = 0
        def racing(*args, **kwargs):
            if args[0] == 'push':
                self.pushes += 1
                if self.pushes == 1 or every:
                    self.advance(self.pushes, own)
                code, output = original(*args, **kwargs)
                return code, (WARNING if warning else '') + output
            return original(*args, **kwargs)
        self.service.git = racing
    def test_real_race_rebases_and_preserves_both_devices(self):
        self.install_race(warning=True)
        with mock.patch.object(base.module.CANCELLED, 'wait', return_value=False):
            self.service.sync()
        self.assertEqual(self.pushes, 2)
        self.assertEqual(git(self.remote, 'show', 'main:ai/raw/wk-me/sessions/session.md'), 'first')
        self.assertEqual(git(self.remote, 'show', 'main:ai/raw/wk-jzx/item.md'), 'remote 1')
        state = self.service.network_state()
        self.assertEqual(state['consecutive_failures'], 0); self.assertFalse(state['paused'])
    def test_repeated_real_races_are_bounded_and_count_one_failure(self):
        self.install_race(every=True)
        with mock.patch.object(base.module.CANCELLED, 'wait', return_value=False):
            with self.assertRaisesRegex(base.module.ServiceError, 'all 3'):
                self.service.sync()
        self.assertEqual(self.pushes, 3)
        self.assertEqual(self.service.network_state()['consecutive_failures'], 1)
        self.assertFalse(self.service.network_state()['paused'])
        self.assertEqual(git(self.service.repo, 'show', 'HEAD:ai/raw/wk-me/sessions/session.md'), 'first')
    def test_real_file_conflict_blocks_without_second_push(self):
        self.service.sync()
        (self.source / 'sessions/session.md').write_text('local edit')
        self.install_race(own=True)
        with mock.patch.object(base.module.CANCELLED, 'wait', return_value=False):
            with self.assertRaisesRegex(base.module.ServiceError, 'blocked'):
                self.service.sync()
        self.assertEqual(self.pushes, 1); self.assertTrue((self.service.state / 'BLOCKED').exists())
        self.assertEqual((self.source / 'sessions/session.md').read_text(), 'local edit')
        self.assertFalse((self.service.repo / '.git/rebase-merge').exists())
    def test_timeout_budget_and_cancellation_stop_reconciliation(self):
        self.service.args.timeout = 1
        original = self.service.git
        self.service.git = lambda *a, **kw: (1, RACE) if a[0] == 'push' else original(*a, **kw)
        with mock.patch.object(base.module.random, 'uniform', return_value=2):
            with self.assertRaisesRegex(base.module.ServiceError, 'time budget'):
                self.service.sync()
        self.assertEqual(self.service.network_state()['consecutive_failures'], 1)
    def test_cancellation_stops_before_retry(self):
        original = self.service.git
        pushes = []
        def reject(*a, **kw):
            if a[0] == 'push':
                pushes.append(a)
                return 1, RACE
            return original(*a, **kw)
        self.service.git = reject
        with mock.patch.object(base.module.CANCELLED, 'wait', return_value=True):
            with self.assertRaisesRegex(base.module.ServiceError, 'Interrupted'):
                self.service.sync()
        self.assertEqual(len(pushes), 1)

    def test_gh001_warning_does_not_pause_or_repair(self):
        original = self.service.git
        self.service.git = lambda *a, **kw: (1, WARNING + 'fatal: connection reset') if a[0] == 'push' else original(*a, **kw)
        with mock.patch.object(self.service, 'recover_large_files', side_effect=AssertionError('must not migrate')):
            with self.assertRaises(base.module.ServiceError): self.service.sync()
        self.assertFalse(self.service.network_state()['paused'])
    def test_successful_push_keeps_warning_in_status(self):
        original = self.service.git
        def warning(*a, **kw):
            code, output = original(*a, **kw)
            return code, WARNING + output if a[0] == 'push' else output
        self.service.git = warning; self.service.sync()
        import json
        self.assertTrue(json.loads((self.service.state/'status.json').read_text())['push_warnings'])


class FrozenRaceTests(unittest.TestCase):
    setUp = daily.TemporaryGitTests.setUp
    run_git = daily.TemporaryGitTests.run_git
    def advance(self, number, own=False, published=False):
        other = self.root / ('other' + str(number))
        self.run_git(self.root, 'clone', str(self.remote), str(other))
        self.run_git(other, 'config', 'user.name', 'Fictional'); self.run_git(other, 'config', 'user.email', 'fictional@example.invalid')
        if published:
            self.run_git(other, 'fetch', str(self.repo), 'HEAD')
            self.run_git(other, 'merge', '--ff-only', 'FETCH_HEAD')
        path = other / ('ai/raw/wk-me/sessions/a.md' if own else 'ai/raw/wk-jzx/item.md')
        path.parent.mkdir(parents=True, exist_ok=True); path.write_text('other ' + str(number))
        self.run_git(other, 'add', '.'); self.run_git(other, 'commit', '-m', 'other device'); self.run_git(other, 'push')
    def install_race(self, every=False, own=False, published=False):
        original = self.s.git; self.pushes = 0; self.frozen = None; self.frozen_tree = None
        def race(*args, **kwargs):
            if args[0] == 'push':
                self.pushes += 1
                if self.pushes == 1:
                    self.frozen = self.run_git(self.repo, 'rev-parse', 'HEAD')
                    self.frozen_tree = self.run_git(self.repo, 'rev-parse', 'HEAD:ai/raw/wk-me')
                    (self.source / 'sessions/a.md').write_text('newer local collection')
                if self.pushes == 1 or every:
                    self.advance(self.pushes, own, published)
            return original(*args, **kwargs)
        self.s.git = race
    def dispatch(self):
        with mock.patch.object(daily.legacy.CANCELLED, 'wait', return_value=False):
            return self.s.dispatch()
    def test_merge_keeps_frozen_payload_and_later_dirty_collection(self):
        (self.source / 'sessions/a.md').write_text('frozen payload'); self.install_race()
        self.assertEqual(self.dispatch(), 'uploaded'); self.assertEqual(self.pushes, 2)
        published = self.run_git(self.remote, 'rev-parse', 'main')
        self.assertNotEqual(published, self.frozen)
        self.assertEqual(self.run_git(self.repo, 'merge-base', '--is-ancestor', self.frozen, published), '')
        self.assertEqual(self.run_git(self.repo, 'rev-parse', published + ':ai/raw/wk-me'), self.frozen_tree)
        self.assertEqual((self.source / 'sessions/a.md').read_text(), 'newer local collection')
        state = daily.ds.read_json(self.s.report_path, {})
        self.assertEqual(state['lastUploadedCommit'], published); self.assertNotIn('pending', state)
        self.assertEqual(self.s.network_state()['consecutive_failures'], 0)
    def test_exhaustion_keeps_original_frozen_and_restarts_with_merge_candidate(self):
        (self.source / 'sessions/a.md').write_text('frozen payload'); self.install_race(every=True)
        with self.assertRaisesRegex(daily.legacy.ServiceError, 'all 3'): self.dispatch()
        pending = copy.deepcopy(daily.ds.read_json(self.s.report_path, {})['pending'])
        self.assertEqual(pending['commit'], self.frozen); self.assertNotEqual(pending['uploadCommit'], self.frozen)
        self.assertEqual(self.s.network_state()['consecutive_failures'], 1)
        self.s.git = self.real_git
        # Keep LFS offline while constructing a new service, simulating restart.
        fresh = daily.service_class(daily.legacy)(self.s.args)
        real = fresh.git
        def offline(*a, **kw):
            if a[0] == 'lfs': fresh.begin_network(); return 0, ''
            return real(*a, **kw)
        fresh.git = offline; self.clock += 3601
        with mock.patch.object(daily.legacy.CANCELLED, 'wait', return_value=False):
            self.assertEqual(fresh.dispatch(), 'uploaded')
        self.assertEqual((self.source / 'sessions/a.md').read_text(), 'newer local collection')
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD:ai/raw/wk-me'), self.frozen_tree)
    def test_remote_own_archive_change_blocks(self):
        (self.source / 'sessions/a.md').write_text('frozen payload'); self.install_race(own=True)
        with self.assertRaisesRegex(daily.legacy.ServiceError, 'sync blocked'): self.dispatch()
        self.assertEqual(self.pushes, 1); self.assertTrue((self.s.state / 'BLOCKED').exists())
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD'), self.frozen)
    def test_remote_already_contains_frozen_commit(self):
        (self.source / 'sessions/a.md').write_text('frozen payload'); self.install_race(published=True)
        self.assertEqual(self.dispatch(), 'uploaded')
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD:ai/raw/wk-me'), self.frozen_tree)
    def test_crash_after_merge_before_queue_save_recovers_exact_merge(self):
        (self.source / 'sessions/a.md').write_text('frozen payload'); self.install_race()
        actual = self.s.recover_integration; crashed = False
        def crash(frozen, pending, save):
            nonlocal crashed
            if pending.get('integration') and not crashed:
                crashed = True
                raise OSError('fictional crash after merge')
            return actual(frozen, pending, save)
        self.s.recover_integration = crash
        with self.assertRaisesRegex(OSError, 'fictional crash'): self.dispatch()
        candidate = self.run_git(self.repo, 'rev-parse', 'HEAD')
        state = daily.ds.read_json(self.s.report_path, {})
        self.assertIn('integration', state['pending']); self.assertNotEqual(candidate, self.frozen)
        self.clock += 3601; self.assertEqual(self.dispatch(), 'uploaded')
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), candidate)
        self.assertEqual((self.source / 'sessions/a.md').read_text(), 'newer local collection')
    def test_actual_size_rejection_preserves_frozen_commit_without_history_repair(self):
        (self.source / 'sessions/a.md').write_text('frozen payload')
        original = self.s.git
        def reject(*args, **kwargs):
            if args[0] == 'push':
                return 1, 'remote: error: GH001: Large files detected.\n!\tHEAD:refs/heads/main\t[remote rejected] (pre-receive hook declined)'
            return original(*args, **kwargs)
        self.s.git = reject
        with self.assertRaises(daily.legacy.ServiceError): self.dispatch()
        pending = daily.ds.read_json(self.s.report_path, {})['pending']
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD'), pending['commit'])
        self.assertTrue(self.s.network_state()['paused'])
        self.assertEqual(self.s.network_state()['consecutive_failures'], 1)
        self.assertFalse((self.s.state / 'lfs-backups').exists())
        self.clock += 86400
        with mock.patch.object(self.s, 'git', side_effect=AssertionError('paused batch must not access Git')):
            self.assertEqual(self.dispatch(), 'deferred')

    def test_true_shared_attributes_merge_conflict_aborts_preserves_dirty_data(self):
        (self.repo / '.gitattributes').write_text('ai/raw/wk-me/sessions/*.raw.jsonl filter=lfs diff=lfs merge=lfs -text\n')
        self.run_git(self.repo, 'add', '.gitattributes')
        self.run_git(self.repo, 'commit', '-m', 'local LFS rule\n\nWaylog-Service-Archive: ai/raw/wk-me')
        (self.source / 'sessions/a.md').write_text('frozen payload')
        original = self.s.git
        pushes = []
        def race(*args, **kwargs):
            if args[0] == 'push':
                pushes.append(args)
                other = self.root / 'foreign'
                self.run_git(self.root, 'clone', str(self.remote), str(other))
                self.run_git(other, 'config', 'user.name', 'Fictional'); self.run_git(other, 'config', 'user.email', 'fictional@example.invalid')
                (other / '.gitattributes').write_text('ai/raw/wk-jzx/sessions/*.raw.jsonl filter=lfs diff=lfs merge=lfs -text\n')
                self.run_git(other, 'add', '.gitattributes'); self.run_git(other, 'commit', '-m', 'foreign LFS rule'); self.run_git(other, 'push')
                (self.source / 'sessions/a.md').write_text('newer dirty collection')
            return original(*args, **kwargs)
        self.s.git = race
        with self.assertRaisesRegex(daily.legacy.ServiceError, 'merge failed'): self.dispatch()
        self.assertEqual(len(pushes), 1)
        self.assertTrue((self.s.state / 'BLOCKED').exists())
        self.assertFalse((self.repo / '.git/MERGE_HEAD').exists())
        self.assertEqual((self.source / 'sessions/a.md').read_text(), 'newer dirty collection')
        self.assertEqual(self.run_git(self.repo, 'diff', '--cached', '--name-only'), '')

    def test_crash_after_fast_forward_recovers_on_new_service(self):
        (self.source / 'sessions/a.md').write_text('frozen payload'); self.install_race(published=True)
        actual = self.s.recover_integration
        def crash(frozen, pending, save):
            if pending.get('integration'):
                raise OSError('fictional crash after fast-forward')
            return actual(frozen, pending, save)
        self.s.recover_integration = crash
        with self.assertRaisesRegex(OSError, 'fast-forward'): self.dispatch()
        candidate = self.run_git(self.repo, 'rev-parse', 'HEAD')
        fresh = daily.service_class(daily.legacy)(self.s.args); original = fresh.git
        def offline(*a, **kw):
            if a[0] == 'lfs': fresh.begin_network(); return 0, ''
            return original(*a, **kw)
        fresh.git = offline; self.clock += 3601
        self.assertEqual(fresh.dispatch(), 'uploaded')
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), candidate)
        self.assertEqual((self.source / 'sessions/a.md').read_text(), 'newer local collection')

    def test_unrelated_head_change_is_not_adopted(self):
        (self.source / 'sessions/a.md').write_text('frozen payload'); self.reject = True
        with self.assertRaises(OSError): self.dispatch()
        self.reject = False
        (self.repo/'unrelated.md').write_text('unrelated'); self.run_git(self.repo,'add','unrelated.md'); self.run_git(self.repo,'commit','-m','manual change')
        self.clock += 3601
        with self.assertRaises(daily.legacy.ServiceError): self.dispatch()


if __name__ == '__main__':
    unittest.main()
