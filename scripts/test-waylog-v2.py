import argparse
import datetime as dt
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
from waylog_schedule import Schedule, ScheduleError

spec = importlib.util.spec_from_file_location('v2', Path(__file__).with_name('waylog-service-v2.py'))
v2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v2)


def epoch(text):
    return dt.datetime.fromisoformat(text).timestamp()


class ScheduleTests(unittest.TestCase):
    def cron(self, expression, zone='Asia/Shanghai'):
        return Schedule({'mode': 'cron', 'expression': expression, 'timezone': zone})

    def test_workdays_and_explicit_zone(self):
        s = self.cron('0 9,12,18 * * 1-5')
        self.assertEqual(s.next_after(epoch('2026-10-16T18:00:00+08:00')), epoch('2026-10-19T09:00:00+08:00'))

    def test_step_lists_and_sunday_alias(self):
        s = self.cron('*/10 8-9 * * 0,7')
        self.assertEqual(s.next_after(epoch('2026-10-18T08:05:00+08:00')), epoch('2026-10-18T08:10:00+08:00'))

    def test_delayed_trigger_is_coalesced_and_duplicate_skipped(self):
        s = self.cron('*/5 * * * *')
        last = epoch('2026-10-14T08:00:00+08:00')
        now = epoch('2026-10-14T09:17:00+08:00')
        slot = s.due_slot(now, last)
        self.assertEqual(slot, epoch('2026-10-14T09:15:00+08:00'))
        self.assertIsNone(s.due_slot(now, slot))

    def test_dst_gap_and_fold_use_utc_minutes(self):
        s = self.cron('30 2 * * *', 'America/New_York')
        self.assertEqual(s.next_after(epoch('2026-03-07T02:30:00-05:00')), epoch('2026-03-09T02:30:00-04:00'))
        s = self.cron('30 1 * * *', 'America/New_York')
        self.assertEqual(s.next_after(epoch('2026-11-01T01:30:00-04:00')), epoch('2026-11-01T01:30:00-05:00'))

    def test_invalid_configuration(self):
        for expr in ['0 0 * *', '60 * * * *', '0 25 * * *', '*/0 * * * *', '0 0 L * *', '-1 * * * *', '0 0 31 2 *']:
            with self.subTest(expr=expr), self.assertRaises(ScheduleError):
                self.cron(expr)
        with self.assertRaises(ScheduleError):
            self.cron('* * * * *', 'Invalid/Nowhere')
        with self.assertRaises(ScheduleError):
            Schedule({'mode': 'interval', 'seconds': True})

    def test_interval_and_native_specs_do_not_register(self):
        s = Schedule(interval=300)
        self.assertEqual(s.next_after(1000), 1300)
        self.assertEqual(s.due_slot(5000, 1000), 4800)
        for platform in ('macos', 'windows'):
            self.assertFalse(s.native_spec(platform)['registrationPerformed'])
            self.assertEqual(s.native_spec(platform)['pollSeconds'], 300)
        self.assertEqual(self.cron('*/5 * * * *').native_spec('windows')['pollSeconds'], 60)

    def test_configuration_isolation_and_mutual_exclusion(self):
        with tempfile.TemporaryDirectory() as root:
            p = Path(root) / 'config.json'
            p.write_text(json.dumps({'interval': 300, 'git_repo': 'fake-repo'}))
            values = v2.load_configuration(p)
            self.assertEqual(values.sync_schedule, {'mode': 'interval', 'seconds': 300})
            self.assertEqual(values.git_repo, str((Path(root) / 'fake-repo').resolve()))
            p.write_text(json.dumps({'interval': 3600, 'sync_schedule': {'mode': 'cron', 'expression': '*/5 * * * *', 'timezone': 'Asia/Shanghai'}}))
            with self.assertRaisesRegex(ValueError, 'mutually exclusive'):
                v2.load_configuration(p)


class ServicePolicyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.config = root / 'fixture-config.json'
        self.config.write_text(json.dumps({'git_repo': str(root / 'repo'), 'state_dir': str(root / 'state'), 'repo_path': 'ai/raw/wk-me',
            'archive_dir': str(root / 'repo/ai/raw/wk-me'),
            'sync_schedule': {'mode': 'cron', 'expression': '*/5 * * * *', 'timezone': 'Asia/Shanghai'}}))
        self.s = v2.Service(v2.load_configuration(self.config))
        self.s.state.mkdir(parents=True)
        self.s.source.mkdir(parents=True)
        self.now = epoch('2026-10-14T09:00:10+08:00')
        self.clock = mock.patch.object(v2.time, 'time', lambda: self.now)
        self.clock.start(); self.addCleanup(self.clock.stop)

    def test_short_cron_success_not_limited_by_legacy_hour(self):
        self.s.begin_network()
        self.s.finish_network(True)
        self.assertEqual(self.s.network_state()['next_retry_epoch'], epoch('2026-10-14T09:05:00+08:00'))
        self.assertEqual(self.s.network_state()['consecutive_failures'], 0)

    def test_failed_short_cron_retains_hour_backoff_and_no_storm(self):
        self.s.begin_network()
        self.s.finish_network(False)
        deadline = self.s.network_state()['next_retry_epoch']
        self.assertGreaterEqual(deadline, self.now + 3600)
        with mock.patch.object(self.s, 'record'), mock.patch('builtins.print'):
            for _ in range(11):
                self.now += 300
                self.assertFalse(self.s.network_allowed())
        self.assertEqual(self.s.network_state()['consecutive_failures'], 1)

    def test_unknown_interrupted_upload_keeps_reservation(self):
        self.s.begin_network()
        state = self.s.network_state()
        self.assertTrue(state['in_flight'])
        fresh = v2.Service(v2.load_configuration(self.config))
        with mock.patch('builtins.print'):
            self.assertFalse(fresh.network_allowed())
        self.assertEqual(fresh.network_state()['consecutive_failures'], 1)

    def test_dispatch_same_slot_does_not_run_twice_and_failure_is_not_replayed(self):
        self.now = epoch('2026-10-14T09:00:00+08:00')
        with mock.patch.object(self.s, 'sync') as sync:
            self.assertEqual(self.s.dispatch(), 'completed')
            self.assertEqual(self.s.dispatch(), 'not-due')
            sync.assert_called_once()
        self.now += 300
        with mock.patch.object(self.s, 'sync', side_effect=RuntimeError('fictional network failure')):
            with self.assertRaises(RuntimeError):
                self.s.dispatch()
        with mock.patch.object(self.s, 'sync') as sync:
            self.assertEqual(self.s.dispatch(), 'not-due'); sync.assert_not_called()

    def test_overlap_is_blocked_before_work(self):
        with self.s.lock('schedule-dispatch'), mock.patch.object(self.s, 'sync') as sync:
            with self.assertRaises(v2.legacy.ServiceError):
                self.s.dispatch()
            sync.assert_not_called()

    def test_git_status_never_changes_coverage_and_version_b_is_not_acknowledged(self):
        coverage = self.s.source / 'indexes/coverage.json'
        coverage.parent.mkdir(); coverage.write_text('{"schemaVersion":1,"fixture":true}')
        files = self.s.source / 'sessions'; files.mkdir()
        p = files / 'fictional.md'; p.write_text('A\n')
        a = self.s.archive_version()
        self.s._uploaded_version = a
        with mock.patch.object(self.s, 'git', return_value=(0, 'fictional-commit')):
            self.s.record(result='success', sync_action='push')
        p.write_text('B\n')
        b = self.s.archive_version(); self.assertNotEqual(a, b)
        state = json.loads((self.s.state / 'status.json').read_text())
        self.assertEqual(state['last_uploaded_data_version'], a)
        self.s.record(result='failed', last_error='fictional fetch failure')
        self.assertEqual(coverage.read_text(), '{"schemaVersion":1,"fixture":true}')
        self.assertNotEqual(state['last_uploaded_data_version'], b)

    def test_native_specs_are_plain_data_no_executor_called(self):
        with mock.patch.object(v2.legacy, 'run', side_effect=AssertionError('no scheduler execution')):
            spec = self.s.native_specs()
        self.assertEqual(spec['macos']['launchd']['StartInterval'], 60)
        self.assertIn('dispatch', spec['macos']['launchd']['ProgramArguments'])
        self.assertEqual(spec['windows']['task']['MultipleInstances'], 'IgnoreNew')
        self.assertTrue(spec['windows']['task']['Execute'].endswith('pythonw.exe'))

    def test_paused_dispatch_never_scans_archive_or_calls_sync(self):
        self.now = epoch('2026-10-14T09:00:00+08:00')
        state = self.s.network_state()
        state.update(paused=True, consecutive_failures=3)
        self.s.save_network_state(state)
        with mock.patch.object(self.s, 'inventory', side_effect=AssertionError('must not scan paused archive')), mock.patch.object(self.s, 'sync') as sync, mock.patch('builtins.print'):
            self.assertEqual(self.s.dispatch(), 'deferred')
            sync.assert_not_called()

    def test_cron_recovery_waits_for_next_slot_and_backoff(self):
        self.now = epoch('2026-10-14T09:00:00+08:00')
        self.s.begin_network(); self.s.finish_network(False)
        with mock.patch.object(self.s, 'sync') as sync, mock.patch('builtins.print'):
            self.now += 300
            self.assertEqual(self.s.dispatch(), 'deferred')
            sync.assert_not_called()
            self.now = epoch('2026-10-14T10:00:00+08:00')
            self.assertEqual(self.s.dispatch(), 'completed')
            sync.assert_called_once()

    def test_source_configuration_is_registered_in_native_watch_spec(self):
        values = json.loads(self.config.read_text()); values.update(collection_sources=['codex','chrome'], coverage_enabled=True)
        self.config.write_text(json.dumps(values)); service = v2.Service(v2.load_configuration(self.config))
        command = service.watch_spec()
        self.assertIn('--coverage', command)
        self.assertEqual(command[command.index('--sources') + 1], 'codex,chrome')
        self.assertIn('--interval-seconds', command)
        values['collection_sources'] = []; self.config.write_text(json.dumps(values))
        service = v2.Service(v2.load_configuration(self.config)); command = service.watch_spec()
        self.assertEqual(command[command.index('--sources') + 1], 'none')

    def test_interval_slot_alignment_survives_scheduler_latency(self):
        s = Schedule(interval=3600)
        first = s.due_slot(epoch('2026-10-14T09:00:05+08:00'))
        self.assertEqual(s.due_slot(epoch('2026-10-14T10:00:01+08:00'), first), epoch('2026-10-14T10:00:00+08:00'))

    def test_windowless_dispatch_worker_logs_without_a_console(self):
        from types import SimpleNamespace
        with mock.patch.object(v2, 'os', SimpleNamespace(name='nt')), mock.patch.object(v2.legacy, 'windows_job', return_value=object()), mock.patch.object(self.s, 'dispatch', side_effect=lambda: print('fictional dispatched')):
            try:
                self.assertEqual(self.s.worker('dispatch'), 0)
            finally:
                v2.legacy.CANCELLED.clear()
        self.assertIn('fictional dispatched', (self.s.state / 'logs/sync-worker.log').read_text())


    def test_unknown_upload_resolved_by_fetched_commit_does_not_acknowledge_new_data(self):
        a = 'sha256:' + 'a' * 64
        self.s.record(upload_result='unknown', upload_attempt_commit='fictional-A', upload_attempt_data_version=a,
                      upload_attempt_archive_tree='tree-A', current_data_version='sha256:' + 'b' * 64)
        with mock.patch.object(v2.legacy.Service, 'git', return_value=(0, '')):
            self.s.record(result='success', sync_action='pull-only')
        state = json.loads((self.s.state / 'status.json').read_text())
        self.assertEqual(state['upload_result'], 'confirmed-by-fetch')
        self.assertEqual(state['last_uploaded_data_version'], a)
        self.assertNotEqual(state['last_uploaded_data_version'], state['current_data_version'])


    def test_old_success_cooldown_can_align_to_new_short_cron_but_failure_cannot(self):
        previous = self.now
        state = {'consecutive_failures': 0, 'paused': False, 'in_flight': False,
                 'last_attempt_epoch': previous, 'next_retry_epoch': previous + 3600}
        (self.s.state / 'network-state.json').write_text(json.dumps(state))
        self.assertEqual(self.s.network_state()['next_retry_epoch'], epoch('2026-10-14T09:05:00+08:00'))
        state['consecutive_failures'] = 1
        (self.s.state / 'network-state.json').write_text(json.dumps(state))
        self.assertEqual(self.s.network_state()['next_retry_epoch'], previous + 3600)
        state.update(consecutive_failures=0, in_flight=True)
        (self.s.state / 'network-state.json').write_text(json.dumps(state))
        self.assertEqual(self.s.network_state()['next_retry_epoch'], previous + 3600)


    def test_old_binary_fails_capability_check_before_any_launch(self):
        with mock.patch.object(v2.legacy.Service, 'check_watch_support'), mock.patch.object(v2.legacy, 'run', return_value=(0, '--archive-lock-file --git-dir')):
            self.s.args.coverage_enabled = True
            with self.assertRaisesRegex(v2.legacy.ServiceError, 'does not support'):
                self.s.check_watch_support()

    def test_invalid_source_selection_is_not_silently_ignored(self):
        data = json.loads(self.config.read_text()); data['collection_sources'] = ['codex', 'made-up-source']
        self.config.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, 'collection_sources'):
            v2.load_configuration(self.config)

    def test_fake_native_executors_only_dispatch_one_claimed_slot(self):
        self.now = epoch('2026-10-14T09:00:00+08:00')
        specs = self.s.native_specs()
        with mock.patch.object(self.s, 'sync') as sync:
            results = [self.s.dispatch() for platform in ('macos', 'windows') for _ in range(3)]
            self.assertEqual(results.count('completed'), 1)
            sync.assert_called_once()
        self.assertFalse(specs['macos']['registrationPerformed'])
        self.assertFalse(specs['windows']['registrationPerformed'])


if __name__ == '__main__':
    unittest.main()
