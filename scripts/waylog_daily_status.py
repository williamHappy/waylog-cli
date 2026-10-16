"""Opt-in operational status. All receipts are local until an upload is queued.

No archive hashing/count reconciliation. The caller holds archive-access.lock for
collection/control and sync.lock for queue/network state. Transport is injectable.
"""
import copy
import datetime as dt
import json
import os
from pathlib import Path
import re
import tempfile
import uuid
from waylog_schedule import Schedule

ZONE = dt.timezone(dt.timedelta(hours=8))
STAGES = {'collection', 'read', 'parse', 'archive'}


def read_json(path, default):
    path = Path(path)
    if path.is_symlink():
        raise ValueError('status path is a symlink')
    if not path.exists():
        return copy.deepcopy(default)
    return json.loads(path.read_text(encoding='utf-8'))


def atomic_json(path, value):
    path = Path(path)
    if any(p.is_symlink() for p in (path, *path.parents)):
        raise ValueError('status path contains a symlink')
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.daily-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8', newline='\n') as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


def instant(text):
    if not isinstance(text, str):
        raise ValueError('checkedAt requires an aware timestamp')
    value = dt.datetime.fromisoformat(text.replace('Z', '+00:00'))
    if value.tzinfo is None:
        raise ValueError('checkedAt requires an aware timestamp')
    return value.astimezone(ZONE)


def validate(document, device, now=None):
    if (type(document.get('schemaVersion')) is not int or document['schemaVersion'] != 1
            or document.get('deviceId') != device or document.get('timezone') != 'Asia/Shanghai'
            or type(document.get('collectionEnabled', True)) is not bool):
        raise ValueError('invalid status identity/control')
    if not isinstance(document.get('sources', {}), dict) or not isinstance(document.get('days', {}), dict):
        raise ValueError('invalid legacy fields')
    checks = document.get('dailyChecks', {})
    if not isinstance(checks, dict):
        raise ValueError('invalid dailyChecks')
    for day, check in checks.items():
        checked = instant(check['checkedAt'])
        if checked.date().isoformat() != day or (now is not None and checked.timestamp() > now):
            raise ValueError('daily check has wrong date or future time')
        if check.get('result') not in ('ok', 'failed') or not isinstance(check.get('sources', {}), dict):
            raise ValueError('invalid daily result')
        for sid, row in check.get('sources', {}).items():
            if not isinstance(sid, str) or not isinstance(row, dict) or row.get('result') not in ('ok', 'failed'):
                raise ValueError('invalid source result')
            if 'stage' in row and row['stage'] not in STAGES:
                raise ValueError('invalid stage')
            if 'reasonCode' in row and not (isinstance(row['reasonCode'], str) and re.fullmatch(r'[a-zA-Z0-9_-]{1,96}', row['reasonCode'])):
                raise ValueError('invalid reasonCode')
    return document


def empty(device):
    return {'schemaVersion': 1, 'deviceId': device, 'timezone': 'Asia/Shanghai',
            'collectionEnabled': True, 'dailyChecks': {}}


def local_document(path, device):
    value = read_json(path, {'controlRevision': 0, 'receipt': empty(device)})
    receipt = value.get('receipt', value)
    validate(receipt, device)
    revision = value.get('controlRevision', 0)
    if type(revision) is not int or revision < 0:
        raise ValueError('invalid local controlRevision')
    return {'schemaVersion': 1, 'deviceId': device, 'receipt': receipt, 'controlRevision': revision}


def set_enabled(path, device, enabled):
    """Explicit control, atomic with its revision. Caller holds archive lock."""
    if type(enabled) is not bool:
        raise ValueError('enabled must be boolean')
    local = local_document(path, device)
    local['receipt']['collectionEnabled'] = enabled
    local['controlRevision'] += 1
    atomic_json(path, local)


def prune(document, today):
    try:
        cutoff = today.replace(year=today.year - 1)
    except ValueError:
        cutoff = today.replace(year=today.year - 1, day=28)
    for field in ('dailyChecks', 'days'):
        if field in document:
            document[field] = {day: value for day, value in document[field].items()
                               if cutoff < dt.date.fromisoformat(day) <= today}


def observe(document, checked_at, updates):
    """Fixture/reference reducer matching native actual-source checks; no fake days."""
    doc = copy.deepcopy(document)
    checked = instant(checked_at)
    validate(doc, doc['deviceId'], checked.timestamp())
    if not updates:
        return doc
    day = checked.date().isoformat()
    known = {}
    for _, check in sorted(doc.get('dailyChecks', {}).items()):
        known.update(check.get('sources', {}))
    known = {sid: row for sid, row in known.items() if row['result'] == 'failed'}
    known.update(doc.get('dailyChecks', {}).get(day, {}).get('sources', {}))
    known.update(copy.deepcopy(updates))
    # Only a real check for the same source can replace its pending error.
    failed = any(row['result'] == 'failed' and doc.get('sources', {}).get(sid, {}).get('enabled') is not False
                 for sid, row in known.items())
    doc.setdefault('dailyChecks', {})[day] = {'checkedAt': checked.isoformat(),
        'result': 'failed' if failed else 'ok', 'sources': known}
    doc.setdefault('collectionEnabled', True)
    prune(doc, checked.date())
    validate(doc, doc['deviceId'], checked.timestamp())
    return doc


def merge_receipt(existing, incoming, now):
    """Preserve old archive fields; merge actual dated observations, never upload time."""
    device = incoming['deviceId']
    validate(incoming, device, now)
    result = copy.deepcopy(existing or empty(device))
    validate(result, device, now)
    for key, value in incoming.items():
        if key not in ('sources', 'days', 'dailyChecks'):
            result[key] = copy.deepcopy(value)
    result.setdefault('sources', {}).update(copy.deepcopy(incoming.get('sources', {})))
    for field in ('days', 'dailyChecks'):
        for day, value in incoming.get(field, {}).items():
            old = result.setdefault(field, {}).get(day)
            if field == 'dailyChecks' and old and instant(old['checkedAt']) > instant(value['checkedAt']):
                continue
            if field == 'days' and old is not None:
                continue  # Lightweight publisher never rewrites legacy archive evidence.
            result[field][day] = copy.deepcopy(value)
    # New operational results supersede stale legacy errors for that same day.
    # Preserve counts/versions, but never promote them into full-day proof.
    for day, check in result.get('dailyChecks', {}).items():
        for sid, row in check.get('sources', {}).items():
            old = result.get('days', {}).get(day, {}).get('sources', {}).get(sid)
            if not old:
                continue
            if old.get('status') in ('failed', 'stopped') or old.get('archiveStatus') == 'failed':
                old['lastFailureReason'] = old.get('reasonCode') or 'previous_failure'
                old.update(status='unknown' if row['result'] == 'ok' else 'failed',
                    archiveStatus='failed' if row.get('stage') == 'archive' else 'unknown',
                    reasonCode='operational_check_ok' if row['result'] == 'ok' else row.get('reasonCode'),
                    successfulRanges=[])
    prune(result, dt.datetime.fromtimestamp(now, ZONE).date())
    return result


def checkpoint_options(options):
    value = dict(options or {'enabled': False, 'time': '23:00'})
    if set(value) != {'enabled', 'time'} or type(value['enabled']) is not bool or not isinstance(value['time'], str) or not re.fullmatch(r'(?:[01]\d|2[0-3]):[0-5]\d', value['time']):
        raise ValueError('checkpoint requires enabled boolean and HH:MM time')
    return value


class Reporter:
    """Transport owns existing bounded retry gate and the archive coordination lock.

    Required methods: locked(), read_local(), has_data_changes(), allowed(),
    freeze(pending, save), upload(commit), pull(), finish(success). freeze must
    durably save the commit before uploading and recover a commit after a crash.
    """
    def __init__(self, state_path, device, schedule, checkpoint, transport, clock):
        self.path, self.device = Path(state_path), device
        self.schedule = Schedule(schedule)
        self.checkpoint = checkpoint_options(checkpoint)
        self.transport, self.clock = transport, clock

    def save(self, state):
        atomic_json(self.path, state)

    def poll(self):
        t = self.transport
        with t.locked():
            # The collector may finish a newer receipt while we wait for its
            # archive lock. Compare against time after acquisition, not entry.
            now = self.clock()
            state = read_json(self.path, {})
            if state.get('schedule') != self.schedule.spec:
                state['schedule'] = self.schedule.spec
                state.pop('slot', None)
            slot = self.schedule.due_slot(now, state.get('slot'))
            regular = slot is not None
            if regular:
                state['slot'] = slot
            if not state.get('pending'):
                local = t.read_local()
                receipt = validate(local['receipt'], self.device, now)
                control = local['controlRevision'] > state.get('acknowledgedControlRevision', 0)
                days = []
                if self.checkpoint['enabled']:
                    hour, minute = map(int, self.checkpoint['time'].split(':'))
                    today = dt.datetime.fromtimestamp(now, ZONE).date().isoformat()
                    candidates = set(receipt.get('dailyChecks', {}))
                    if receipt.get('collectionEnabled', True):
                        candidates.add(today)  # Report unknown honestly when no actual check exists.
                    for day in sorted(candidates):
                        due = dt.datetime.combine(dt.date.fromisoformat(day), dt.time(hour, minute), ZONE).timestamp()
                        if now >= due and day not in state.get('completed', {}):
                            days.append(day)
                data = regular and t.has_data_changes()
                if data or control or days:
                    latest = max(receipt.get('dailyChecks', {}), default=None)
                    failed = any(r.get('stage') == 'archive' and r['result'] == 'failed' for sid, r in receipt.get('dailyChecks', {}).get(latest, {}).get('sources', {}).items() if receipt.get('sources', {}).get(sid, {}).get('enabled') is not False)
                    state['pending'] = {'receipt': copy.deepcopy(receipt), 'checkpointDays': days,
                        'controlRevision': local['controlRevision'], 'includeData': bool(data and not failed),
                        'queuedAt': now, 'reportId': str(uuid.uuid4())}
                self.save(state)
            pending = state.get('pending')
            if not pending:
                if regular and t.pull_on_idle and t.allowed():
                    try:
                        t.pull()
                        t.finish(True)
                    except Exception:
                        t.finish(False)
                        raise
                    return 'pull-only'
                return 'idle'
            if not t.allowed():
                self.save(state)
                return 'deferred'
            try:
                def saved():
                    self.save(state)
                if 'commit' not in pending:
                    t.freeze(pending, saved)
                t.upload(pending['commit'])
                t.finish(True)
            except Exception:
                t.finish(False)
                state['lastUploadResult'] = 'failed'  # LOCAL ONLY, no timestamp in coverage.
                self.save(state)
                raise
            state.setdefault('completed', {}).update({day: pending['commit'] for day in pending['checkpointDays']})
            state['acknowledgedControlRevision'] = pending['controlRevision']
            state['lastUploadedCommit'] = pending['commit']
            state['lastUploadResult'] = 'success'
            state.pop('pending')
            cutoff_doc = {'days': state['completed']}
            prune(cutoff_doc, dt.datetime.fromtimestamp(now, ZONE).date())
            state['completed'] = cutoff_doc['days']
            self.save(state)
            return 'uploaded'
