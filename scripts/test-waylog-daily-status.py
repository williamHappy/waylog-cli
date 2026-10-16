#!/usr/bin/env python3
"""Only fictional records, fake clock/upload and temporary local bare Git remotes."""
import argparse
import contextlib
import copy
import datetime as dt
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import waylog_daily_status as ds
from generate_daily_status_fixtures import generate
from waylog_light_transport import service_class

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('daily_v2', ROOT / 'scripts/waylog-service-v2.py')
v2 = importlib.util.module_from_spec(spec); spec.loader.exec_module(v2)
legacy = v2.legacy


def epoch(text): return ds.instant(text).timestamp()
def ok(): return {'result': 'ok'}
def failed(stage='read'): return {'result': 'failed', 'stage': stage, 'reasonCode': 'fictional_failure'}


class FakeTransport:
    pull_on_idle = False
    def __init__(self):
        self.local = {'receipt': ds.observe(ds.empty('wk-me'), '2026-10-14T10:00:00+08:00', {'session:codex': ok()}), 'controlRevision': 0}
        self.data = False; self.attempts = []; self.payloads = []; self.pulls = 0
        self.fail = False; self.allow = True; self.finishes = []; self.freeze_count = 0
    def locked(self): return contextlib.nullcontext()
    def read_local(self): return copy.deepcopy(self.local)
    def has_data_changes(self): return self.data
    def allowed(self): return self.allow
    def finish(self, success): self.finishes.append(success)
    def pull(self): self.pulls += 1
    def freeze(self, p, save):
        self.freeze_count += 1; p['commit'] = 'frozen-' + str(self.freeze_count)
        self.payloads.append(copy.deepcopy(p)); save()
        if p['includeData']: self.data = False
    def upload(self, commit):
        self.attempts.append(commit)
        if self.fail: raise OSError('fictional upload failed')


class DailyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='waylog-daily-fictional-'); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve(); self.now = epoch('2026-10-14T10:00:00+08:00')
        self.t = FakeTransport(); self.path = self.root / 'report.json'
        self.reporter = ds.Reporter(self.path, 'wk-me', {'mode':'interval','seconds':3600}, {'enabled':True,'time':'23:00'}, self.t, lambda:self.now)
    def state(self): return ds.read_json(self.path, {})
    def poll(self, text=None):
        if text:self.now=epoch(text)
        return self.reporter.poll()
    def test_lock_wait_uses_acquisition_time_including_midnight(self):
        for before, after in [('2026-10-14T10:00:00+08:00', '2026-10-14T10:00:30+08:00'),
                              ('2026-10-14T23:59:59+08:00', '2026-10-15T00:00:02+08:00')]:
            with self.subTest(after=after):
                self.path.unlink(missing_ok=True)
                self.now = epoch(before)
                @contextlib.contextmanager
                def delayed_lock():
                    self.now = epoch(after)
                    self.t.local['receipt'] = ds.observe(ds.empty('wk-me'), after, {'session:codex': ok()})
                    yield
                self.t.locked = delayed_lock
                self.t.data = True
                self.assertEqual(self.poll(), 'uploaded')
                check = self.t.payloads[-1]['receipt']['dailyChecks'][after[:10]]
                self.assertEqual(check['checkedAt'], after)

    def test_idle_no_upload_no_checkpoint_before_time(self):
        self.assertEqual(self.poll(), 'idle'); self.assertEqual(self.t.freeze_count,0)
        self.assertEqual(self.poll(), 'idle')
    def test_data_carries_status_daily_checkpoint_independent_deduplicated(self):
        self.t.data=True; self.assertEqual(self.poll(),'uploaded')
        self.assertEqual(self.state()['completed'],{})
        self.assertEqual(self.t.payloads[0]['receipt']['dailyChecks'],self.t.local['receipt']['dailyChecks'])
        self.assertEqual(self.poll('2026-10-14T23:00:00+08:00'),'uploaded')
        self.assertFalse(self.t.payloads[-1]['includeData'])
        self.assertEqual(self.poll(),'idle'); self.assertEqual(len(self.t.attempts),2)
    def test_failure_frozen_retry_and_no_failure_in_coverage(self):
        self.t.fail=True
        with self.assertRaises(OSError):self.poll('2026-10-14T23:00:00+08:00')
        self.assertNotIn('2026-10-14', self.state().get('completed',{}))
        first=copy.deepcopy(self.state()['pending']); self.t.allow=False
        self.t.local['receipt']=ds.observe(self.t.local['receipt'],'2026-10-15T01:00:00+08:00',{'session:codex':ok()})
        self.t.data=True
        self.assertEqual(self.poll('2026-10-15T01:00:00+08:00'),'deferred')
        self.t.allow=True; self.t.fail=False; self.assertEqual(self.poll(),'uploaded')
        self.assertEqual(self.t.attempts,['frozen-1','frozen-1']); self.assertEqual(self.t.freeze_count,1)
        self.assertEqual(first['receipt'],self.t.payloads[0]['receipt'])
        self.assertNotIn('lastUploadResult', first['receipt']); self.assertNotIn('2026-10-15', self.state()['completed'])
    def test_checkpoint_can_report_unknown_without_fabricating_check(self):
        self.t.local['receipt']=ds.empty('wk-me')
        self.assertEqual(self.poll('2026-10-14T23:00:00+08:00'),'uploaded')
        self.assertFalse(self.t.payloads[-1]['receipt']['dailyChecks'])
        self.assertIn('2026-10-14',self.state()['completed'])
        self.assertEqual(self.poll(),'idle')

    def test_actual_zero_and_no_checks(self):
        d=ds.empty('wk-me'); self.assertFalse(ds.observe(d,'2026-10-14T10:00:00+08:00',{})['dailyChecks'])
        d=ds.observe(d,'2026-10-14T10:00:00+08:00',{'session:codex':ok()})
        self.assertEqual(d['dailyChecks']['2026-10-14']['result'],'ok'); self.assertNotIn('2026-10-13',d['dailyChecks'])
    def test_failures_sibling_and_same_source_recovery(self):
        for stage in sorted(ds.STAGES):
            with self.subTest(stage=stage):
                d=ds.observe(ds.empty('wk-me'),'2026-10-14T10:00:00+08:00',{'session:codex':failed(stage)})
                d=ds.observe(d,'2026-10-15T09:00:00+08:00',{'session:claude':ok()})
                self.assertEqual(d['dailyChecks']['2026-10-15']['result'],'failed')
                d=ds.observe(d,'2026-10-15T10:00:00+08:00',{'session:codex':ok()})
                self.assertEqual(d['dailyChecks']['2026-10-15']['result'],'ok')
    def test_disabled_restore_controls_without_data_or_check(self):
        self.t.local['receipt']=ds.empty('wk-me'); self.t.local['receipt']['collectionEnabled']=False;self.t.local['controlRevision']=1
        self.assertEqual(self.poll(),'uploaded');self.assertFalse(self.t.payloads[-1]['receipt']['collectionEnabled'])
        self.t.local['receipt']['collectionEnabled']=True;self.t.local['controlRevision']=2
        self.assertEqual(self.poll(),'uploaded');self.assertTrue(self.t.payloads[-1]['receipt']['collectionEnabled'])
        self.assertEqual(self.poll(),'idle'); self.assertFalse(self.state()['completed'])
    def test_control_respects_pause(self):
        self.t.allow=False;self.t.local['controlRevision']=1
        self.assertEqual(self.poll(),'deferred');self.assertFalse(self.t.attempts)
    def test_collector_failure_still_reported_without_staging_broken_data(self):
        self.t.local['receipt']=ds.observe(self.t.local['receipt'],'2026-10-14T11:00:00+08:00',{'session:codex':failed('archive')})
        self.t.data=True
        self.assertEqual(self.poll('2026-10-14T23:00:00+08:00'),'uploaded')
        self.assertFalse(self.t.payloads[-1]['includeData']); self.assertTrue(self.t.data)
        self.assertEqual(self.t.payloads[-1]['receipt']['dailyChecks']['2026-10-14']['result'],'failed')
    def test_read_failure_can_attach_other_valid_data(self):
        self.t.local['receipt']=ds.observe(self.t.local['receipt'],'2026-10-14T11:00:00+08:00',{'session:codex':failed('read')})
        self.t.data=True;self.assertEqual(self.poll('2026-10-14T11:00:00+08:00'),'uploaded');self.assertTrue(self.t.payloads[-1]['includeData'])
    def test_idle_mac_pulls_windows_skips(self):
        self.assertEqual(self.poll(),'idle');self.assertEqual(self.t.pulls,0)
        self.t.pull_on_idle=True
        self.assertEqual(self.poll('2026-10-14T11:00:00+08:00'),'pull-only');self.assertEqual(self.t.pulls,1)
    def test_backlog_preserves_day_and_checkpoint_before_regular_cron(self):
        self.reporter.schedule=ds.Schedule({'mode':'cron','expression':'0 8 * * *','timezone':'Asia/Shanghai'})
        self.assertEqual(self.poll('2026-10-15T01:17:00+08:00'),'uploaded')
        self.assertEqual(set(self.state()['completed']),{'2026-10-14'})
        self.assertEqual(self.t.payloads[0]['receipt']['dailyChecks']['2026-10-14']['checkedAt'],'2026-10-14T10:00:00+08:00')
        self.assertEqual(self.poll(),'idle')
    def test_retention_and_disabled_roster(self):
        d=ds.observe(ds.empty('wk-me'),'2025-10-15T10:00:00+08:00',{'session:codex':failed()})
        d['sources']={'session:codex':{'enabled':False}}
        d=ds.observe(d,'2026-10-15T10:00:00+08:00',{'session:claude':ok()})
        self.assertEqual(list(d['dailyChecks']),['2026-10-15']);self.assertEqual(d['dailyChecks']['2026-10-15']['result'],'ok')
    def test_merge_preserves_legacy_fields_resolves_old_failure(self):
        old=ds.empty('wk-me');old['days']={'2026-10-14':{'dataVersion':'sha256:'+'a'*64,'sources':{'session:codex':{'status':'failed','archiveStatus':'failed','reasonCode':'old_failure','recordCount':9,'successfulRanges':[]}}}}
        old['extension_preserved']={'opaque':True}
        result=ds.merge_receipt(old,self.t.local['receipt'],self.now)
        self.assertEqual(result['extension_preserved'],old['extension_preserved']);row=result['days']['2026-10-14']['sources']['session:codex']
        self.assertEqual(row['recordCount'],9);self.assertEqual(row['status'],'unknown');self.assertEqual(row['lastFailureReason'],'old_failure')
    def test_invalid_future_naive_date_reason_stage(self):
        for mutation in ('future','naive','date','reason','stage','enabled'):
            d=copy.deepcopy(self.t.local['receipt']);check=d['dailyChecks']['2026-10-14']
            if mutation=='future':check['checkedAt']='2026-10-14T23:00:00+08:00'
            if mutation=='naive':check['checkedAt']='2026-10-14T10:00:00'
            if mutation=='date':check['checkedAt']='2026-10-15T10:00:00+08:00'
            if mutation=='reason':check['sources']['session:codex']['reasonCode']='bad.code'
            if mutation=='stage':check['sources']['session:codex']['stage']=None
            if mutation=='enabled':d['collectionEnabled']=None
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):ds.validate(d,'wk-me',self.now)
    def test_local_control_atomic_no_fake_check(self):
        p=self.root/'local.json';ds.set_enabled(p,'wk-me',False)
        local=ds.local_document(p,'wk-me');self.assertFalse(local['receipt']['collectionEnabled']);self.assertEqual(local['controlRevision'],1)
        self.assertFalse(local['receipt']['dailyChecks']);ds.set_enabled(p,'wk-me',True);self.assertEqual(ds.local_document(p,'wk-me')['controlRevision'],2)


class TemporaryGitTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(prefix='waylog-light-git-fictional-');self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.remote=self.root/'remote.git';self.repo=self.root/'repo';self.clock=epoch('2026-10-14T23:00:00+08:00')
        self.run_git(self.root,'init','--bare','--initial-branch=main',str(self.remote))
        self.run_git(self.root,'clone',str(self.remote),str(self.repo));self.run_git(self.repo,'config','user.name','Fictional');self.run_git(self.repo,'config','user.email','fictional@example.invalid')
        (self.repo/'README.md').write_text('fictional\n');self.run_git(self.repo,'add','.');self.run_git(self.repo,'commit','-m','seed');self.run_git(self.repo,'push','origin','main')
        self.source=self.repo/'ai/raw/wk-me';(self.source/'sessions').mkdir(parents=True);(self.source/'indexes').mkdir()
        args=argparse.Namespace(git_repo=str(self.repo),archive_dir=str(self.source),state_dir=str(self.root/'state'),repo_path='ai/raw/wk-me',repo_mode='full',remote=str(self.remote),branch='main',interval=3600,minimum_network_interval=3600,timeout=10,git_name='Fictional',git_email='fictional@example.invalid',waylog='/unused',no_browser=True,pull_on_idle=False,retry_max_failures=3,retry_delays=[3600,7200],sync_schedule={'mode':'interval','seconds':3600},daily_status={'enabled':True,'checkpoint':{'enabled':True,'time':'23:00'}})
        self.s=service_class(legacy)(args);self.s.state.mkdir()
        ds.atomic_json(self.s.local_path,{'receipt':ds.observe(ds.empty('wk-me'),'2026-10-14T22:00:00+08:00',{'session:codex':ok()}),'controlRevision':0})
        self.calls=[];self.real_git=self.s.git;self.reject=False
        def git(*args,**kwargs):
            self.calls.append(args)
            if args[0]=='lfs':self.s.begin_network();return 0,'' # Mock upload; no LFS server.
            if args[0]=='push' and self.reject:
                self.s.begin_network();raise OSError('fictional connection lost')
            return self.real_git(*args,**kwargs)
        self.s.git=git
        patch=mock.patch.object(legacy.time,'time',side_effect=lambda:self.clock);patch.start();self.addCleanup(patch.stop)
    def run_git(self,root,*args):
        return subprocess.check_output(['git','-C',str(root),*args],stderr=subprocess.STDOUT,text=True).strip()
    def test_data_status_same_commit_retry_exact_head_and_bounded_pause(self):
        data=self.source/'sessions/fictional.md';data.write_text('fictional data\n');self.reject=True
        with self.assertRaises(OSError):self.s.dispatch()
        head=self.run_git(self.repo,'rev-parse','HEAD');first=ds.read_json(self.s.report_path,{})['pending'];self.assertEqual(first['commit'],head)
        self.assertEqual(self.run_git(self.repo,'show',head+':ai/raw/wk-me/sessions/fictional.md'),'fictional data')
        self.assertEqual(json.loads(self.run_git(self.repo,'show',head+':ai/raw/wk-me/indexes/coverage.json'))['dailyChecks']['2026-10-14']['result'],'ok')
        ds.atomic_json(self.s.local_path,{'receipt':ds.observe(ds.empty('wk-me'),'2026-10-15T00:30:00+08:00',{'session:codex':ok()}),'controlRevision':0})
        data.write_text('later local data\n')
        self.assertEqual(self.s.dispatch(),'deferred')
        for wait in (3601,7201):
            self.clock+=wait
            with self.assertRaises(OSError):self.s.dispatch()
            self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),head)
        self.clock+=86400;before=len(self.calls);self.assertEqual(self.s.dispatch(),'deferred')
        self.assertFalse(any(c[0] in ('fetch','push','lfs') for c in self.calls[before:]))
        self.assertEqual(self.s.network_state()['consecutive_failures'],3);self.assertTrue(self.s.network_state()['paused'])
        self.assertNotIn('lastUploadResult',json.loads((self.source/'indexes/coverage.json').read_text()))
    def test_successful_retry_reuses_commit_preserves_newer_local_files(self):
        file=self.source/'sessions/a.md';file.write_text('initial fictional\n');self.reject=True
        with self.assertRaises(OSError):self.s.dispatch()
        head=self.run_git(self.repo,'rev-parse','HEAD');self.reject=False
        file.write_text('later fictional\n');self.clock+=3601;start=len(self.calls)
        self.assertEqual(self.s.dispatch(),'uploaded');self.assertEqual(self.run_git(self.remote,'rev-parse','main'),head)
        self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),head)
        self.assertEqual(file.read_text(),'later fictional\n')
        self.assertFalse(any(c[0] in ('fetch','commit') for c in self.calls[start:]))

    def test_crash_after_commit_before_commit_id_save_recovers_same_head(self):
        (self.source/'sessions/a.md').write_text('fictional\n');self.reject=True
        with self.assertRaises(OSError):self.s.dispatch()
        head=self.run_git(self.repo,'rev-parse','HEAD');state=ds.read_json(self.s.report_path,{})
        state['pending'].pop('commit');ds.atomic_json(self.s.report_path,state)
        self.clock+=3601;self.reject=False
        self.assertEqual(self.s.dispatch(),'uploaded')
        self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),head)

    def test_new_local_heartbeat_without_data_does_not_create_commit(self):
        (self.source/'sessions/a.md').write_text('fictional\n');self.assertEqual(self.s.dispatch(),'uploaded')
        head=self.run_git(self.repo,'rev-parse','HEAD');coverage=(self.source/'indexes/coverage.json').read_bytes()
        self.clock+=3601
        ds.atomic_json(self.s.local_path,{'receipt':ds.observe(ds.empty('wk-me'),'2026-10-15T00:00:00+08:00',{'session:codex':ok()}),'controlRevision':0})
        start=len(self.calls);self.assertEqual(self.s.dispatch(),'idle')
        self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),head)
        self.assertEqual((self.source/'indexes/coverage.json').read_bytes(),coverage)
        self.assertFalse(any(c[0] in ('fetch','push','lfs') for c in self.calls[start:]))

    def test_success_and_no_empty_commit_for_repeat_checkpoint(self):
        (self.source/'sessions/fictional.md').write_text('fictional\n');self.assertEqual(self.s.dispatch(),'uploaded')
        head=self.run_git(self.repo,'rev-parse','HEAD');self.assertEqual(self.run_git(self.remote,'rev-parse','main'),head)
        self.assertEqual(self.s.dispatch(),'idle');self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),head)
    def test_archive_failure_status_only_preserves_uncommitted_data(self):
        (self.source/'sessions/broken.md').write_text('fictional incomplete\n')
        ds.atomic_json(self.s.local_path,{'receipt':ds.observe(ds.empty('wk-me'),'2026-10-14T22:00:00+08:00',{'session:codex':failed('archive')}),'controlRevision':0})
        self.assertEqual(self.s.dispatch(),'uploaded');head=self.run_git(self.repo,'rev-parse','HEAD')
        files=self.run_git(self.repo,'ls-tree','-r','--name-only',head)
        self.assertNotIn('broken.md',files);self.assertTrue((self.source/'sessions/broken.md').exists());self.assertIn('coverage.json',files)
    def test_daily_checkpoint_earlier_upload_no_new_commit_if_receipt_identical(self):
        self.clock=epoch('2026-10-14T22:30:00+08:00');(self.source/'sessions/a.md').write_text('fictional\n')
        self.assertEqual(self.s.dispatch(),'uploaded');head=self.run_git(self.repo,'rev-parse','HEAD')
        self.clock=epoch('2026-10-14T23:00:00+08:00');self.assertEqual(self.s.dispatch(),'uploaded')
        self.assertEqual(self.run_git(self.repo,'rev-parse','HEAD'),head)
        self.assertIn('2026-10-14',ds.read_json(self.s.report_path,{})['completed'])


class ConfigurationTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(prefix='waylog-config-fictional-');self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve();self.path=self.root/'config.json'
        self.config={'git_repo':str(self.root/'repo'),'archive_dir':str(self.root/'repo/ai/raw/wk-me'),
            'repo_path':'ai/raw/wk-me','state_dir':str(self.root/'state'),
            'daily_status':{'enabled':True,'checkpoint':{'enabled':True,'time':'23:17'}}}
    def load(self):
        ds.atomic_json(self.path,self.config);return v2.load_configuration(self.path)
    def test_native_specs_only_poll_and_use_native_light_watch(self):
        service=v2.Service(self.load());spec=service.native_specs()
        self.assertIn('--daily-status-file',service.watch_spec());self.assertNotIn('--coverage',service.watch_spec())
        self.assertEqual(spec['macos']['launchd']['StartInterval'],60)
        self.assertNotIn('StartCalendarInterval',spec['macos']['launchd'])
        self.assertEqual(spec['windows']['task']['RepetitionSeconds'],60)
        self.assertFalse(spec['macos']['registrationPerformed'])
    def test_windows_bundle_includes_light_import_dependencies(self):
        workflow=(ROOT/'.github/workflows/windows-test.yml').read_text()
        copy_line=next(line for line in workflow.splitlines() if 'Copy-Item scripts/waylog-service.py,' in line)
        self.assertIn('scripts/waylog_daily_status.py',copy_line)
        self.assertIn('scripts/waylog_light_transport.py',copy_line)

    def test_default_off_and_reject_strict_light_combination(self):
        self.config.pop('daily_status');options=self.load();self.assertFalse(options.daily_status['enabled'])
        self.config['daily_status']={'enabled':True,'checkpoint':{'enabled':True,'time':'23:17'}}
        self.config['coverage_enabled']=True
        with self.assertRaises(ValueError):self.load()
    def test_light_dispatch_never_calls_digest_audit(self):
        service=v2.Service(self.load())
        with mock.patch.object(v2,'service_class') as factory, mock.patch.object(service,'archive_version',side_effect=AssertionError('no digest')):
            factory.return_value.return_value.dispatch.return_value='idle'
            self.assertEqual(service.dispatch(),'idle')


class FixtureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='waylog-schema-fixtures-')
        self.addCleanup(temporary.cleanup)
        self.fixtures = generate(Path(temporary.name).resolve() / 'fixtures')

    def test_all_fixtures_schema_and_consumer(self):
        try:import jsonschema
        except ImportError:jsonschema=None
        consumer=Path('/Users/wangct/workspace/wk-me/i-ai/iHermes-webui/skills/personal-daily/scripts')
        module=None
        if (consumer/'coverage_files.py').is_file():
            sys.path.insert(0,str(consumer))
            try:module=__import__('coverage_files')
            finally:sys.path.remove(str(consumer))
        count=0
        for path in sorted(self.fixtures.glob('*/i-raw-data/ai/raw/*/indexes/coverage.json')):
            doc=json.loads(path.read_text());device=path.parents[1].name
            ds.validate(doc,device,epoch('2026-10-15T23:59:59+08:00'))
            if jsonschema:
                jsonschema.Draft202012Validator(json.loads((ROOT/'docs/coverage-v1.schema.json').read_text()),format_checker=jsonschema.FormatChecker()).validate(doc)
            if module:
                result=module.annual_observation(doc,device,'2026-10-14',ds.instant('2026-10-15T23:59:59+08:00'))
                case=path.parents[5].name
                expected='disabled' if case=='disabled' else 'failed' if case in ('collection-failure','read-failure','parse-failure','archive-failure','sibling-success','next-day-backlog') else 'unknown' if case=='missing-check' else 'success'
                self.assertEqual(result['collectionStatus'],expected,(case,result))
            count+=1
        self.assertEqual(count,24)
        print('Fictional contracts:',count,'schema:',bool(jsonschema),'consumer:',bool(module))


if __name__=='__main__':unittest.main(verbosity=2)
