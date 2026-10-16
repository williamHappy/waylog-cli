#!/usr/bin/env python3
"""Generate only fictional lightweight consumer archives; never read real inputs."""
import argparse
import json
import tempfile
from pathlib import Path
from waylog_daily_status import empty, observe

CASES = ('normal', 'zero-records', 'collection-failure', 'read-failure', 'parse-failure',
         'archive-failure', 'sibling-success', 'same-source-recovered', 'disabled', 'resumed',
         'missing-check', 'next-day-backlog')


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def generate(output):
    output = Path(output).expanduser()
    if output.is_symlink():
        raise ValueError('Fixture output must not be a symlink')
    output = output.resolve()
    if output.exists() and any(output.iterdir()):
        raise ValueError('Fixture output must be a new or empty directory')
    for case in CASES:
        for device in ('wk-me', 'wk-jzx'):
            root = output / case / 'i-raw-data'
            archive = root / 'ai/raw' / device
            indexes = archive / 'indexes'
            sessions = archive / 'sessions'
            sessions.mkdir(parents=True, exist_ok=True)
            indexes.mkdir(parents=True, exist_ok=True)
            browser = archive / 'browser-history'
            browser.mkdir(parents=True, exist_ok=True)
            # Keep empty required directories intact when copied or checked out.
            (sessions / '.gitkeep').write_text('', encoding='utf-8')
            (browser / '.gitkeep').write_text('', encoding='utf-8')
            c = empty(device)
            c['sources'] = {sid: {'kind': 'session', 'enabled': True, 'discovery': 'fictional'}
                            for sid in ('session:codex', 'session:claude')}
            if case != 'missing-check':
                stage = case.removesuffix('-failure') if case.endswith('-failure') else None
                row = {'result': 'failed', 'stage': stage, 'reasonCode': 'fictional_' + stage + '_failed'} if stage else {'result': 'ok'}
                if case in ('sibling-success', 'same-source-recovered', 'next-day-backlog'):
                    row = {'result': 'failed', 'stage': 'read', 'reasonCode': 'fictional_read_failed'}
                c = observe(c, '2026-10-14T10:00:03+08:00', {'session:codex': row})
                if case in ('sibling-success', 'same-source-recovered'):
                    c = observe(c, '2026-10-14T11:00:00+08:00', {'session:claude': {'result': 'ok'}})
                if case == 'same-source-recovered':
                    c = observe(c, '2026-10-14T12:00:00+08:00', {'session:codex': {'result': 'ok'}})
                if case == 'next-day-backlog':
                    c = observe(c, '2026-10-15T09:00:00+08:00', {'session:claude': {'result': 'ok'}})
                if case == 'disabled': c['collectionEnabled'] = False
                if case == 'resumed': c['collectionEnabled'] = True
            write(indexes / 'coverage.json', c)
            # Full tiny archive layout for consumers. Zero is an actual successful
            # source read with no records; missing-check means no actual check.
            entries = []
            if case not in ('zero-records', 'missing-check'):
                stem = '2026-10-14_10-00-00_codex_Fictional-session_0123456789ab'
                md, raw = sessions / (stem + '.md'), sessions / (stem + '.raw.jsonl')
                md.write_text(
                    '---\nprovider: codex\nsession_id: fictional-session\n'
                    'project: /fictional/project\nstarted_at: 2026-10-14T10:00:00+08:00\n'
                    'updated_at: 2026-10-14T10:00:01+08:00\nmessage_count: 1\n---\n\n'
                    '# Fictional session\n\n'
                    '## 👤 User (2026-10-14 10:00:01 +08:00)\n\n'
                    'Fictional example only.\n\n\n', encoding='utf-8')
                events = [{'timestamp': '2026-10-14T02:00:00Z', 'type': 'session_meta', 'payload': {'id': 'fictional-session', 'cwd': '/fictional/project'}},
                          {'timestamp': '2026-10-14T02:00:01Z', 'type': 'response_item', 'payload': {'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': 'Fictional example only.'}]}}]
                raw.write_text(''.join(json.dumps(e) + '\n' for e in events), encoding='utf-8')
                entries.append({'stable_key': 'codex:fictional-session', 'provider': 'codex', 'session_id': 'fictional-session', 'title': 'Fictional session',
                    'started_at': '2026-10-14T10:00:00+08:00', 'project_path': '/fictional/project', 'source_path': '/fictional/source.jsonl',
                    'source_mtime': '2026-10-14T10:00:01+08:00', 'source_size': raw.stat().st_size, 'message_count': 1,
                    'markdown_path': str(md), 'raw_path': str(raw), 'exported_at': '2026-10-14T10:00:02+08:00'})
            (indexes / 'sessions.jsonl').write_text(''.join(json.dumps(e) + '\n' for e in entries), encoding='utf-8')
            (indexes / 'browser-history.jsonl').write_text('', encoding='utf-8')
            manifest = {'archive_version': 3, 'session_count': len(entries), 'browser_record_count': 0, 'providers': ['codex'] if entries else [], 'browser_sources': [], 'latest_session': 'codex:fictional-session' if entries else None, 'updated_at': '2026-10-14T10:00:02+08:00'}
            write(indexes / 'manifest.json', manifest)
            write(archive / 'manifest.json', manifest)
        write(output / case / 'fixture.json', {'fictional': True, 'case': case, 'targetDay': '2026-10-14',
            'boundary': '2026-10-15T23:59:59+08:00', 'devices': ['wk-me', 'wk-jzx']})
    return output


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', help='New or empty destination; default: a fresh system temporary directory')
    args = parser.parse_args()
    destination = args.output or tempfile.mkdtemp(prefix='waylog-daily-samples-')
    print(generate(destination))
