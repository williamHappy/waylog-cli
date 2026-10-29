#!/usr/bin/env python3
"""Real local Git/LFS objects; all remotes are disposable file-system repos."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import shlex
import sys
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('lfs_daily', Path(__file__).with_name('test-waylog-daily-status.py'))
daily = importlib.util.module_from_spec(spec); spec.loader.exec_module(daily)
from waylog_frozen_lfs import FrozenLFS


class RecoveryTests(unittest.TestCase):
    setUp = daily.TemporaryGitTests.setUp
    run_git = daily.TemporaryGitTests.run_git

    def install_rejection(self, paths=None, crash=None, advance=False):
        self.file = self.source / 'sessions/a.md'
        self.file.write_bytes(b'fictional frozen bytes\n' * 16)
        self.original_bytes = self.file.read_bytes()
        self.published = self.run_git(self.remote, 'rev-parse', 'main')
        self.s.args.timeout = 30
        self.pushes = 0
        original = self.s.git
        paths = paths or ['ai/raw/wk-me/sessions/a.md']
        def reject(*a, **kw):
            if a[0] == 'push':
                self.pushes += 1
                if self.pushes == 1 and advance:
                    other = self.root/'other'
                    self.run_git(self.root, 'clone', str(self.remote), str(other))
                    self.run_git(other, 'config', 'user.name', 'Other')
                    self.run_git(other, 'config', 'user.email', 'other@example.invalid')
                    (other/'README.md').write_text('other device edit\n')
                    self.run_git(other, 'add', '.'); self.run_git(other, 'commit', '-m', 'other device')
                    self.run_git(other, 'push')
                    self.published = self.run_git(self.remote, 'rev-parse', 'main')
                    return original(*a, **kw)
                if self.pushes == (2 if advance else 1):
                    self.old = self.run_git(self.repo, 'rev-parse', 'HEAD')
                    self.file.write_bytes(b'newer uncommitted collector bytes\n')
                    self.newer = self.file.read_bytes()
                    lines = ''.join('remote: error: File ' + p + ' is 101.00 MB; this exceeds limit\n' for p in paths)
                    return 1, lines + 'remote: error: GH001: Large files detected.\n'
            return original(*a, **kw)
        self.s.git = reject
        if crash:
            real = FrozenLFS.git
            def interrupt(recovery, *a, **kw):
                result = real(recovery, *a, **kw)
                if a[0] == crash and 'env' not in kw and not getattr(self, 'crashed', False):
                    self.crashed = True
                    raise OSError('fictional crash after ' + crash)
                return result
            patch = mock.patch.object(FrozenLFS, 'git', interrupt)
            patch.start(); self.addCleanup(patch.stop)

    def pending(self):
        return daily.ds.read_json(self.s.report_path, {})['pending']

    def run_first(self):
        with mock.patch.object(daily.legacy.CANCELLED, 'wait', return_value=False):
            with self.assertRaises(Exception):
                self.s.dispatch()

    def test_repair_and_next_cycle_publish_real_lfs_content(self):
        self.install_rejection()
        self.run_first()
        pending = self.pending()
        self.assertEqual(pending['lfsRecovery']['phase'], 'complete')
        self.assertNotEqual(pending['commit'], self.old)
        self.assertEqual(self.pushes, 1)  # No immediate re-push.
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), self.published)
        self.assertEqual(self.file.read_bytes(), self.newer)
        pointer = self.run_git(self.repo, 'show', pending['commit']+':ai/raw/wk-me/sessions/a.md')
        sha = hashlib.sha256(self.original_bytes).hexdigest()
        self.assertIn('oid sha256:'+sha, pointer)
        self.assertIn('size '+str(len(self.original_bytes)), pointer)
        env = self.run_git(self.repo, 'lfs', 'env')
        media = next(x.split('=',1)[1] for x in env.splitlines() if x.startswith('LocalMediaDir='))
        self.assertEqual((Path(media)/sha[:2]/sha[2:4]/sha).read_bytes(), self.original_bytes)
        self.assertEqual(self.run_git(self.repo, 'show', self.old+':ai/raw/wk-me/sessions/a.md'), self.original_bytes.decode().strip())
        self.run_git(self.repo, 'bundle', 'verify', pending['lfsRecovery']['backup'])
        original = json.loads((Path(pending['lfsRecovery']['backup']).parent/'original.json').read_text())['pending']
        for field in ('receipt','checkpointDays','controlRevision','includeData','queuedAt','reportId'):
            self.assertEqual(pending.get(field), original.get(field))
        self.assertFalse(self.s.network_state()['paused'])
        self.assertEqual(self.s.network_state()['consecutive_failures'], 1)
        # Exercise actual Git LFS upload to the local bare repository (no server).
        self.run_git(self.repo, 'lfs', 'push', 'origin', pending['commit'])
        self.clock += 3601
        self.assertEqual(self.s.dispatch(), 'uploaded')
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), pending['commit'])
        self.assertEqual(self.file.read_bytes(), self.newer)
        self.assertEqual(self.s.network_state()['consecutive_failures'], 0)
        self.assertEqual(self.run_git(self.repo, 'merge-base', '--is-ancestor', self.published, 'HEAD'), '')
        remote_object = self.remote/'lfs/objects'/sha[:2]/sha[2:4]/sha
        self.assertEqual(remote_object.read_bytes(), self.original_bytes)

    def test_later_dirty_file_does_not_enter_migrated_frozen_commit(self):
        self.install_rejection(); self.run_first()
        self.assertEqual(self.file.read_bytes(), self.newer)
        self.assertNotIn(self.newer.decode().strip(), self.run_git(self.repo, 'show', self.pending()['commit']+':ai/raw/wk-me/sessions/a.md'))
        self.assertEqual(self.run_git(self.repo, 'diff', '--cached', '--name-only'), '')

    def fresh(self):
        fresh = daily.service_class(daily.legacy)(self.s.args)
        real = fresh.git
        def offline(*a, **kw):
            if a[0] == 'lfs': fresh.begin_network(); return 0, ''
            return real(*a, **kw)
        fresh.git = offline
        return fresh

    def test_restart_after_ref_update_finishes_transaction_before_pause_gate(self):
        self.install_rejection(crash='update-ref'); self.run_first()
        p = copy.deepcopy(self.pending()); self.assertEqual(p['lfsRecovery']['phase'], 'prepared')
        self.assertTrue(self.s.network_state()['paused'])
        fresh = self.fresh()
        self.assertEqual(fresh.dispatch(), 'deferred')  # Finishes locally, keeps cooldown.
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'complete')
        self.assertFalse(fresh.network_state()['paused'])
        self.assertEqual(self.file.read_bytes(), self.newer)
        self.clock += 3601
        self.assertEqual(fresh.dispatch(), 'uploaded')
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), p['lfsRecovery']['newCandidate'])

    def test_crash_after_index_update_recovers(self):
        self.install_rejection(crash='read-tree'); self.run_first()
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'prepared')
        self.assertTrue(self.s.network_state()['paused'])
        self.assertEqual(self.file.read_bytes(), self.newer)
        self.assertEqual(self.fresh().dispatch(), 'deferred')
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'complete')
        self.assertEqual(self.run_git(self.repo, 'diff', '--cached', '--name-only'), '')

    def test_foreign_rejected_path_refuses_and_keeps_batch(self):
        self.install_rejection(paths=['ai/raw/wk-jzx/sessions/a.md']); self.run_first()
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD'), self.old)
        self.assertNotIn('lfsRecovery', self.pending())
        self.assertTrue(self.s.network_state()['paused'])
        self.assertEqual(self.file.read_bytes(), self.newer)

    def test_remote_race_merge_candidate_is_mapped_without_rewriting_published_parent(self):
        self.install_rejection(advance=True); self.run_first()
        p = self.pending(); self.assertEqual(p['lfsRecovery']['phase'], 'complete')
        self.assertNotEqual(p['commit'], p['uploadCommit'])
        self.assertEqual(self.run_git(self.repo, 'merge-base', '--is-ancestor', self.published, p['uploadCommit']), '')
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), self.published)
        self.clock += 3601
        self.assertEqual(self.s.dispatch(), 'uploaded')
        self.assertEqual(self.run_git(self.remote, 'show', 'main:README.md'), 'other device edit')
        self.assertEqual(self.file.read_bytes(), self.newer)

    def test_retry_limit_pause_is_not_cleared_by_successful_local_repair(self):
        self.install_rejection()
        network = self.s.network_state(); network['consecutive_failures'] = 2
        self.s.save_network_state(network)
        self.run_first()
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'complete')
        self.assertTrue(self.s.network_state()['paused'])
        self.assertEqual(self.s.network_state()['consecutive_failures'], 3)

    def test_real_local_rejection_hook_then_lfs_upload_without_force(self):
        file = self.source/'sessions/a.md'
        with file.open('wb') as stream:
            for _ in range(101):
                stream.write(b'x' * (1024 * 1024))
        published = self.run_git(self.remote, 'rev-parse', 'main')
        hook = self.remote/'hooks/pre-receive'
        hook_script = self.root/'reject-large.py'
        hook_script.write_text('''import subprocess,sys
for line in sys.stdin:
    old,new,ref=line.split()
    p='ai/raw/wk-me/sessions/a.md'
    r=subprocess.run(['git','cat-file','-s',new+':'+p],capture_output=True,text=True)
    if r.returncode == 0 and int(r.stdout)>1024:
        print('error: File '+p+' is 101.00 MB; this exceeds the test limit', flush=True)
        print('error: GH001: Large files detected.', flush=True)
        sys.exit(1)
''')
        hook.write_text('#!/bin/sh\nexec ' + shlex.quote(sys.executable.replace('\\', '/')) + ' ' + shlex.quote(str(hook_script).replace('\\', '/')) + '\n')
        hook.chmod(0o755)
        self.s.args.timeout = 30
        self.run_first()
        p=self.pending(); self.assertEqual(p['lfsRecovery']['phase'], 'complete')
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), published)
        self.run_git(self.repo, 'lfs', 'push', 'origin', p['commit'])
        self.clock += 3601
        self.assertEqual(self.s.dispatch(), 'uploaded')
        self.assertEqual(self.run_git(self.remote, 'rev-parse', 'main'), p['commit'])
        self.assertFalse(any('--force' in arg or arg.startswith('+') for call in self.calls for arg in call))

    def test_crash_before_ref_update_recovers_from_saved_intent(self):
        self.install_rejection()
        with mock.patch.object(FrozenLFS, 'resume', side_effect=OSError('before ref update')):
            self.run_first()
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD'), self.old)
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'prepared')
        self.assertEqual(self.fresh().dispatch(), 'deferred')
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'complete')
        self.assertEqual(self.file.read_bytes(), self.newer)

    def test_backup_failure_leaves_original_history_and_pause(self):
        self.install_rejection()
        original = FrozenLFS.git
        def fail(recovery, *a, **kw):
            if a[:2] == ('bundle','verify'): raise OSError('backup verification failed')
            return original(recovery, *a, **kw)
        with mock.patch.object(FrozenLFS,'git',fail): self.run_first()
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD'), self.old)
        self.assertNotIn('lfsRecovery',self.pending())
        self.assertTrue(self.s.network_state()['paused'])

    def test_tag_pointing_at_unpublished_batch_prevents_rewrite(self):
        self.install_rejection()
        original = FrozenLFS.prepare
        def tagged(recovery, output):
            self.run_git(self.repo,'tag','protected-tag','HEAD')
            return original(recovery,output)
        with mock.patch.object(FrozenLFS,'prepare',tagged): self.run_first()
        self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),self.old)
        self.assertEqual(self.run_git(self.repo,'rev-parse','protected-tag'),self.old)
        self.assertTrue(self.s.network_state()['paused'])

    def test_unknown_head_after_crash_refuses_to_finish_transaction(self):
        self.install_rejection(crash='update-ref'); self.run_first()
        # Move only the branch to another known object, simulating external interference.
        self.run_git(self.repo,'update-ref','refs/heads/main',self.published)
        before=self.run_git(self.repo,'rev-parse','HEAD')
        with self.assertRaisesRegex(daily.legacy.ServiceError,'HEAD changed'):
            self.fresh().dispatch()
        self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),before)
        self.assertTrue(self.s.network_state()['paused'])

    def test_completed_journal_does_not_clear_a_later_gh001_pause(self):
        self.install_rejection();self.run_first()
        n=self.s.network_state();n['paused']=True;n['pause_reason']='GitHub GH001 rejected large files; later unsafe path'
        self.s.save_network_state(n)
        self.assertEqual(self.fresh().dispatch(),'deferred')
        self.assertTrue(self.s.network_state()['paused'])

    def test_sparse_checkout_preserves_layout_and_later_dirty_content(self):
        self.run_git(self.repo,'sparse-checkout','init','--cone')
        self.run_git(self.repo,'sparse-checkout','set','ai/raw/wk-me')
        self.run_git(self.repo,'config','lfs.fetchinclude','ai/raw/wk-me/**')
        self.run_git(self.repo,'config','remote.origin.partialclonefilter','blob:none')
        self.run_git(self.repo,'config','remote.origin.promisor','true')
        self.s.repo_mode=self.s.args.repo_mode='sparse'
        self.install_rejection();self.run_first()
        self.assertEqual(self.pending()['lfsRecovery']['phase'],'complete')
        self.assertEqual(self.run_git(self.repo,'sparse-checkout','list'),'ai/raw/wk-me')
        self.assertEqual(self.file.read_bytes(),self.newer)
        self.clock+=3601
        self.assertEqual(self.s.dispatch(),'uploaded')


    def test_process_exit_after_queue_update_before_pause_clear_recovers(self):
        self.install_rejection()
        original = self.s.save_network_state
        def power_loss(network):
            p = daily.ds.read_json(self.s.report_path, {}).get('pending', {})
            if p.get('lfsRecovery', {}).get('phase') == 'applied' and not network.get('paused'):
                raise SystemExit('power loss after queue update')
            return original(network)
        with mock.patch.object(self.s, 'save_network_state', power_loss):
            with self.assertRaises(SystemExit): self.s.dispatch()
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'applied')
        fresh = self.fresh()
        self.assertEqual(fresh.dispatch(), 'deferred')
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'complete')
        self.assertFalse(fresh.network_state()['paused'])
        self.assertFalse(fresh.network_state()['in_flight'])
        self.assertEqual(self.file.read_bytes(), self.newer)

    def cached_object(self, content):
        sha = hashlib.sha256(self.original_bytes).hexdigest()
        media = next(line.split('=', 1)[1] for line in self.run_git(self.repo, 'lfs', 'env').splitlines() if line.startswith('LocalMediaDir='))
        target = Path(media)/sha[:2]/sha[2:4]/sha
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
        return target

    def test_valid_readonly_existing_lfs_cache_is_reused(self):
        self.install_rejection()
        cache = self.cached_object(self.original_bytes)
        cache.chmod(0o444)
        self.addCleanup(lambda: cache.chmod(0o644) if cache.exists() else None)
        self.run_first()
        self.assertEqual(self.pending()['lfsRecovery']['phase'], 'complete')
        self.assertEqual(cache.read_bytes(), self.original_bytes)

    def test_corrupt_existing_lfs_cache_leaves_original_commit_paused(self):
        self.install_rejection()
        cache = self.cached_object(b'corrupt cache')
        self.run_first()
        self.assertEqual(self.run_git(self.repo, 'rev-parse', 'HEAD'), self.old)
        self.assertTrue(self.s.network_state()['paused'])
        self.assertEqual(cache.read_bytes(), b'corrupt cache')


if __name__ == '__main__': unittest.main()
