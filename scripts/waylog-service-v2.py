#!/usr/bin/env python3
"""Opt-in entry point. Existing service scripts/tasks are deliberately unchanged."""
import argparse
import contextlib
import logging
import threading
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import time
from waylog_schedule import Schedule, ScheduleError
from waylog_daily_status import checkpoint_options
from waylog_light_transport import service_class

_spec = importlib.util.spec_from_file_location('waylog_legacy', Path(__file__).with_name('waylog-service.py'))
legacy = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(legacy)


def load_configuration(path):
    path = Path(path).resolve()
    data = legacy.parse_configuration(path.read_text(encoding='utf-8-sig'))
    if not isinstance(data, dict):
        raise ValueError('configuration must be an object')
    data = dict(data)
    daily_status = data.pop('daily_status', {'enabled': False, 'checkpoint': {'enabled': False, 'time': '23:00'}})
    if not isinstance(daily_status, dict) or set(daily_status) != {'enabled', 'checkpoint'} or type(daily_status['enabled']) is not bool:
        raise ValueError('daily_status requires enabled boolean and checkpoint')
    daily_status['checkpoint'] = checkpoint_options(daily_status['checkpoint'])
    schedule_spec = data.pop('sync_schedule', None)
    coverage_enabled = data.pop('coverage_enabled', False)
    collection_sources = data.pop('collection_sources', None)
    if collection_sources is not None and (not isinstance(collection_sources, list) or any(s not in ('codex', 'claude', 'gemini', 'chrome', 'atlas') for s in collection_sources) or len(set(collection_sources)) != len(collection_sources)):
        raise ValueError('collection_sources must be a unique list of supported source IDs')
    if daily_status['enabled'] and coverage_enabled:
        raise ValueError('daily_status and strict coverage_enabled are mutually exclusive')
    if type(coverage_enabled) is not bool:
        raise ValueError('coverage_enabled must be boolean')
    if schedule_spec is not None and 'interval' in data:
        raise ValueError('interval and sync_schedule are mutually exclusive')
    schedule = Schedule(schedule_spec, data.get('interval', 3600))
    # Let the existing validator handle all legacy options, using only a temporary
    # configuration. Resolve paths against the caller's file, not this temp file.
    for name in ('git_repo', 'archive_dir', 'state_dir', 'waylog'):
        if data.get(name) is not None:
            data[name] = legacy.expand_config_path(data[name], path.parent)
    for name in ('python', 'git', 'ssh'):
        if isinstance(data.get(name), str) and any(mark in data[name] for mark in ('/', '\\', '%', '~', '$')):
            data[name] = legacy.expand_config_path(data[name], path.parent)
    with tempfile.TemporaryDirectory(prefix='waylog-v2-config-') as root:
        temporary = Path(root) / 'config.json'
        temporary.write_text(json.dumps(data), encoding='utf-8')
        values, _ = legacy.load_configuration(temporary)
    if schedule.mode == 'interval':
        values['interval'] = schedule.seconds
    values.update(daily_status=daily_status, sync_schedule=schedule.spec, coverage_enabled=coverage_enabled, collection_sources=collection_sources,
                  config_path=str(path), config_overrides={})
    return argparse.Namespace(**values)


class Service(legacy.Service):
    def __init__(self, args):
        super().__init__(args)
        self.schedule = Schedule(args.sync_schedule)
        # Failures keep the established minimum/backoff. Successes follow the
        # explicit schedule, rather than the old default interval floor.
        if self.schedule.mode == 'interval':
            self.network_spacing = max(args.minimum_network_interval, self.schedule.seconds)
        else:
            self.network_spacing = args.minimum_network_interval
        self._v2_slot = None
        self._push_started = False
        self._uploaded_version = None

    def network_state(self):
        data = super().network_state()
        # Only a confirmed successful old cooldown can be reinterpreted after
        # an explicit schedule change. Failure, pause and unknown reservations
        # always keep their original deadline.
        if (self.schedule.mode == 'cron' and data['consecutive_failures'] == 0
                and not data['in_flight'] and not data['paused']
                and data.get('success_schedule') != self.schedule.spec and data['last_attempt_epoch'] > 0):
            aligned = self.schedule.next_after(data['last_attempt_epoch'])
            data['next_retry_epoch'] = min(data['next_retry_epoch'], aligned)
        return data

    def save_network_state(self, data):
        if data['consecutive_failures'] == 0 and not data['in_flight'] and not data['paused']:
            data['success_schedule'] = self.schedule.spec
        super().save_network_state(data)

    def success_deadline(self, attempt_epoch):
        if self.schedule.mode == 'cron':
            return self.schedule.next_after(time.time())
        # Native hourly tasks run at Minute=0 on both platforms. Launch/fetch
        # latency must not make the next successful hourly slot miss its gate.
        # Longer minimum spacing and all failure reservations remain unchanged.
        if self.schedule.seconds == 3600 and self.network_spacing == 3600:
            return (int(time.time()) // 3600 + 1) * 3600
        return super().success_deadline(attempt_epoch) if self.schedule.seconds == self.args.interval else attempt_epoch + self.schedule.seconds

    def command_arguments(self, command):
        return [os.sys.executable, str(Path(__file__).resolve()), '--config', self.args.config_path, command]

    def validate_snapshot(self, root):
        super().validate_snapshot(root)
        path = root / 'indexes/coverage.json'
        if path.exists():
            receipt = json.loads(path.read_text(encoding='utf-8'))
            if receipt.get('schemaVersion') != 1 or receipt.get('deviceId') != self.remote_path.name:
                raise legacy.ServiceError('Coverage schema/device does not match this archive')

    def archive_version(self):
        files = self.inventory(self.source)
        h = hashlib.sha256()
        for path, digest in sorted((Path(path).as_posix(), digest) for path, digest in files.items()):
            if not (path.startswith(('sessions/', 'browser-history/')) or path in ('indexes/sessions.jsonl', 'indexes/browser-history.jsonl')):
                continue
            h.update(path.encode('utf-8') + b'\0' + digest.encode('ascii') + b'\n')
        return 'sha256:' + h.hexdigest()

    def git(self, *args, check=True):
        if args and args[0] == 'push':
            self._push_started = True
            version = self.archive_version()
            commit = super().git('rev-parse', 'HEAD')[1]
            tree = super().git('rev-parse', f'HEAD:{self.remote_path.as_posix()}')[1]
            self.record(upload_attempt_data_version=version, upload_attempt_commit=commit, upload_attempt_archive_tree=tree, upload_result='in-flight')
            result = super().git(*args, check=check)
            if result[0] == 0:
                self._uploaded_version = version
            return result
        return super().git(*args, check=check)

    def record(self, **updates):
        # Git receipts are strictly local, and refer to an immutable pushed tree,
        # not whichever newer files the collector may produce afterwards.
        if updates.get('result') == 'success' and updates.get('sync_action') == 'push':
            updates['last_uploaded_commit'] = self.git('rev-parse', 'HEAD')[1]
            tree = self.git('rev-parse', f'HEAD:{self.remote_path.as_posix()}')[1]
            updates['last_uploaded_archive_tree'] = tree
            updates['last_uploaded_data_version'] = self._uploaded_version
            updates['upload_result'] = 'success'
            self._push_started = False
        elif updates.get('result') == 'success' and updates.get('sync_action') == 'pull-only':
            path = self.state / 'status.json'
            saved = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}
            attempted = saved.get('upload_attempt_commit')
            if saved.get('upload_result') == 'unknown' and attempted:
                code, _ = super().git('merge-base', '--is-ancestor', attempted, f'origin/{self.args.branch}', check=False)
                if code == 0:
                    updates.update(upload_result='confirmed-by-fetch', last_uploaded_commit=attempted,
                        last_uploaded_data_version=saved.get('upload_attempt_data_version'),
                        last_uploaded_archive_tree=saved.get('upload_attempt_archive_tree'))
        elif updates.get('result') == 'failed' and self._push_started:
            updates['upload_result'] = 'unknown'
            self._push_started = False
        super().record(**updates)

    def dispatch(self):
        if getattr(self.args, "daily_status", {}).get("enabled"):
            return service_class(legacy)(self.args).dispatch()
        # Acquire before checking/claiming a slot, so overlapping native triggers
        # cannot start concurrent work or consume extra failure attempts.
        with self.lock('schedule-dispatch'):
            p = self.state / 'schedule-state.json'
            saved = json.loads(p.read_text(encoding='utf-8')) if p.exists() else {}
            if saved.get('schedule') != self.schedule.spec:
                saved = {}
            slot = self.schedule.due_slot(time.time(), saved.get('last_slot'))
            if slot is None:
                return 'not-due'
            # A slot is claimed even if its operation fails or the worker dies.
            # Recovery waits for the next scheduled slot AND the retry gate.
            saved = {'schemaVersion': 1, 'schedule': self.schedule.spec, 'last_slot': slot,
                     'claimed_at': datetime.datetime.now(datetime.timezone.utc).isoformat()}
            temporary = p.with_suffix('.tmp')
            temporary.write_text(json.dumps(saved, indent=2) + '\n', encoding='utf-8')
            temporary.replace(p)
            if not self.network_allowed():
                return 'deferred'
            with self.archive_access():
                pending_version = self.archive_version()
            self.record(pending_data_version=pending_version, schedule_slot=slot)
            self.sync()
            with self.archive_access():
                current_version = self.archive_version()
            # A successful A upload never silently acknowledges current B.
            self.record(current_data_version=current_version, pending_data_version=current_version)
            return 'completed'

    def watch_spec(self):
        command = [self.args.waylog, '--verbose', 'watch', '--archive-dir', str(self.source),
                   '--interval-seconds', str(self.watch_interval), '--archive-lock-file', str(self.state / 'archive-access.lock'),
                   '--git-dir', str(self.repo / '.git')]
        if self.args.no_browser:
            command.append('--no-browser')
        if self.args.collection_sources is not None:
            command += ['--sources', ','.join(self.args.collection_sources) or 'none']
        if getattr(self.args, 'daily_status', {}).get('enabled'):
            command += ['--daily-status-file', str(self.state / 'daily-status.json')]
        if self.args.coverage_enabled:
            command += ['--coverage', '--coverage-device', self.remote_path.name]
        return command

    def check_watch_support(self):
        super().check_watch_support()
        if getattr(self.args, 'daily_status', {}).get('enabled'):
            text = legacy.run([self.args.waylog, 'watch', '--help'])[1]
            if '--daily-status-file' not in text:
                raise legacy.ServiceError('Configured binary lacks --daily-status-file support')
        if self.args.coverage_enabled or self.args.collection_sources is not None:
            text = legacy.run([self.args.waylog, 'watch', '--help'])[1]
            if (self.args.coverage_enabled and '--coverage' not in text) or (self.args.collection_sources is not None and '--sources' not in text):
                raise legacy.ServiceError('Configured binary does not support opt-in coverage/source selection')

    def watch_command(self):
        self.check_watch_support()
        command = self.watch_spec()
        command[command.index('--git-dir') + 1] = self.git('rev-parse', '--absolute-git-dir')[1]
        return command

    def worker(self, kind):
        if kind == 'watch':
            return super().worker('watch')
        if kind != 'dispatch' or os.name != 'nt':
            raise legacy.ServiceError('dispatch-worker requires Windows')
        logs = self.state / 'logs'
        logs.mkdir(parents=True, exist_ok=True)
        logger = logging.getLogger('v2-dispatch-worker')
        logger.setLevel(logging.INFO)
        handler = legacy.RotatingFileHandler(logs / 'sync-worker.log', maxBytes=5 * 1024 * 1024, backupCount=3, encoding='utf-8')
        logger.addHandler(handler)
        class Stream:
            def write(self, text):
                if text.strip():
                    logger.info('%s %s', legacy.stamp(), text.rstrip())
            def flush(self):
                pass
        try:
            with contextlib.redirect_stdout(Stream()), contextlib.redirect_stderr(Stream()):
                job = legacy.windows_job()
                def monitor():
                    while not legacy.CANCELLED.wait(0.5):
                        if (self.state / 'sync.stop').exists():
                            legacy.CANCELLED.set()
                thread = threading.Thread(target=monitor, daemon=True)
                thread.start()
                try:
                    if (self.state / 'sync.stop').exists():
                        return 0
                    self.dispatch()
                    return 0
                except (legacy.ServiceError, OSError, ValueError) as error:
                    print('ERROR:', error)
                    return 1
                finally:
                    legacy.CANCELLED.set()
                    thread.join(timeout=1)
        finally:
            logger.removeHandler(handler)
            handler.close()

    def native_specs(self):
        result = {platform: self.schedule.native_spec(platform) for platform in ('macos', 'windows')}
        light_daily = getattr(self.args, 'daily_status', {}).get('enabled') and self.args.daily_status['checkpoint']['enabled']
        if light_daily:
            for spec in result.values():
                spec['pollSeconds'] = 60
        arguments = self.command_arguments('dispatch')
        result['macos']['launchd'] = {'ProgramArguments': arguments, 'StartInterval': result['macos']['pollSeconds'],
            'RunAtLoad': True, 'WorkingDirectory': str(self.state)}
        if self.schedule.mode == 'interval' and self.schedule.seconds == 3600 and not light_daily:
            result['macos']['launchd'].pop('StartInterval')
            result['macos']['launchd']['StartCalendarInterval'] = {'Minute': 0}
        result['macos']['watch'] = {'ProgramArguments': self.watch_spec() + ['--instance-lock-file', str(self.state / 'watch.lock'), '--service-log-file', str(self.state / 'logs/watch-output.log')], 'KeepAlive': True, 'ThrottleInterval': 30}
        result['windows']['watch'] = {'Execute': str(Path(os.sys.executable).with_name('pythonw.exe')), 'Arguments': legacy.subprocess.list2cmdline(self.command_arguments('watch-worker')[1:]), 'AtLogOn': True, 'RestartCount': 3, 'RestartIntervalSeconds': 60}
        result['windows']['task'] = {'Execute': str(Path(os.sys.executable).with_name('pythonw.exe')),
            'Arguments': legacy.subprocess.list2cmdline(self.command_arguments('dispatch-worker')[1:]), 'RepetitionSeconds': result['windows']['pollSeconds'],
            'MultipleInstances': 'IgnoreNew', 'StartWhenAvailable': True, 'LogonType': 'Interactive'}
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('command', choices=('config', 'native-spec', 'dispatch', 'dispatch-worker', 'watch-worker', 'sync', 'doctor', 'status', 'collection-disable', 'collection-enable', 'resume-sync'))
    args = parser.parse_args()
    options = load_configuration(args.config)
    service = Service(options)
    if args.command == 'config':
        print(json.dumps(vars(options), indent=2))
    elif args.command == 'native-spec':
        print(json.dumps(service.native_specs(), indent=2))
    elif args.command == 'dispatch':
        print(service.dispatch())
    elif args.command in ('dispatch-worker', 'watch-worker'):
        raise SystemExit(service.worker(args.command.split('-')[0]))
    elif args.command in ('collection-disable', 'collection-enable'):
        if not options.daily_status['enabled']:
            raise ValueError('Explicit control requires daily_status.enabled')
        service_class(legacy)(options).control(args.command == 'collection-enable')
    elif args.command == 'resume-sync':
        service.resume_sync()
    elif args.command == 'sync':
        if options.daily_status['enabled']:
            print(service.dispatch())
        else:
            service.sync()
    else:
        getattr(service, args.command)()


if __name__ == '__main__':
    main()
