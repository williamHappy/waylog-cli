#!/usr/bin/env python3
"""End-to-end fictional archives: read_raw -> consume_coverage_files.

Consumer code is imported read-only. Each case is copied as delivered into a
resolved temporary directory. No source logs, config, services, Git or network.
Set WAYLOG_DAILY_CONSUMER_SCRIPTS to use another local consumer checkout.
"""
import datetime as dt
import importlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest import mock
from generate_daily_status_fixtures import CASES, generate

CONSUMER = Path(os.environ.get('WAYLOG_DAILY_CONSUMER_SCRIPTS',
    '/Users/wangct/workspace/wk-me/i-ai/iHermes-webui/skills/personal-daily/scripts'))
BOUNDARY = dt.datetime.fromisoformat('2026-10-15T23:59:59+08:00')
FAILED = {'collection-failure': 'collection', 'read-failure': 'read',
          'parse-failure': 'parse', 'archive-failure': 'archive',
          'sibling-success': 'read', 'next-day-backlog': 'read'}


class FictionalClock(dt.datetime):
    @classmethod
    def now(cls, tz=None):
        return BOUNDARY.astimezone(tz) if tz else BOUNDARY.replace(tzinfo=None)


class CompleteFixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = tempfile.TemporaryDirectory(prefix='waylog-consumer-fixtures-')
        cls.addClassCleanup(temporary.cleanup)
        cls.fixtures = generate(Path(temporary.name).resolve() / 'fixtures')
        # Missing consumer is a clear failure, never an unreported skipped check.
        if not (CONSUMER / 'raw_reader.py').is_file() or not (CONSUMER / 'coverage_files.py').is_file():
            raise RuntimeError('Set WAYLOG_DAILY_CONSUMER_SCRIPTS to the existing daily consumer scripts')
        sys.path.insert(0, str(CONSUMER))
        try:
            cls.reader = importlib.import_module('raw_reader')
            cls.coverage = importlib.import_module('coverage_files')
        finally:
            sys.path.remove(str(CONSUMER))

    def check_case(self, case):
        original = self.fixtures / case / 'i-raw-data'
        with tempfile.TemporaryDirectory(prefix='waylog-complete-fictional-') as temporary:
            root = Path(temporary).resolve() / 'i-raw-data'
            # No mkdir/repair after copying: the delivered layout itself must work.
            shutil.copytree(original, root)
            options = {'repositoryPath': str(root), 'date': '2026-10-14',
                       'timezone': 'Asia/Shanghai', 'groups': ['wk-me', 'wk-jzx']}
            with mock.patch.object(self.reader.dt, 'datetime', FictionalClock):
                raw = self.reader.read_raw(options)
                result = self.coverage.consume_coverage_files({'groups': options['groups']}, raw, str(root))
            expected_raw = 'empty' if case in ('zero-records', 'missing-check') else 'complete'
            self.assertEqual(raw['status'], expected_raw, (case, raw))
            self.assertFalse(raw['warnings'], (case, raw['warnings']))
            for device in options['groups']:
                self.assertEqual(raw['counts'][device]['archiveConsistency'], 'checked')
                self.assertEqual(raw['counts'][device]['sessionMessages'], 0 if expected_raw == 'empty' else 1)
                self.assertEqual(raw['counts'][device]['browserVisits'], 0)
                # Copied index bytes are unchanged; source-machine absolute paths
                # remain metadata and are mapped by the consumer's own contract.
                relative = Path('ai/raw') / device / 'indexes/sessions.jsonl'
                self.assertEqual((root / relative).read_bytes(), (original / relative).read_bytes())
            expected = 'failed' if case in FAILED else 'unknown' if case == 'missing-check' else 'complete'
            self.assertEqual(result['status'], expected, (case, result))
            self.assertFalse(result['archiveDigestChecked'])
            self.assertEqual(result['proofLevel'], 'operational_status_only')
            if case in FAILED:
                self.assertFalse(result['usable'])
                self.assertEqual(result['reason'], 'collection_failed')
                self.assertEqual(len(result['issues']), 2)
                self.assertTrue(all(issue['code'] == 'collection_failed' and issue['stage'] == FAILED[case]
                                    for issue in result['issues']), result)
            elif case == 'missing-check':
                self.assertTrue(result['usable'])
                self.assertTrue(all(issue['code'] == 'daily_check_missing' for issue in result['issues']))
            else:
                self.assertTrue(result['usable'])
                self.assertFalse(result['issues'], result)
            devices = result['deviceResults']
            if case == 'disabled':
                self.assertTrue(all(d['collectionEnabled'] is False and d['collectionStatus'] == 'disabled' for d in devices))
            if case in ('resumed', 'same-source-recovered', 'normal', 'zero-records'):
                self.assertTrue(all(d['collectionEnabled'] is True and d['collectionStatus'] == 'success' for d in devices))
            for event in raw['events']:
                self.assertEqual(event['timestamp'], '2026-10-14T10:00:01+08:00')
            # No input-error substitute is accepted for an intended source failure.
            self.assertNotIn('source_input_unavailable', {issue['code'] for issue in result['issues']})

    def test_message_times_match_raw_and_indexes_and_empty_directories_survive_copy(self):
        for case in CASES:
            for device in ('wk-me', 'wk-jzx'):
                archive = self.fixtures / case / 'i-raw-data/ai/raw' / device
                self.assertTrue((archive / 'browser-history/.gitkeep').is_file())
                self.assertTrue((archive / 'sessions/.gitkeep').is_file())
                for line in (archive / 'indexes/sessions.jsonl').read_text(encoding='utf-8').splitlines():
                    row = json.loads(line)
                    text = Path(row['markdown_path']).read_text(encoding='utf-8')
                    blocks = self.reader.Reader({'repositoryPath': str(self.fixtures / case / 'i-raw-data'),
                                                'date': '2026-10-14'}).session_blocks(text)
                    events = [json.loads(v) for v in Path(row['raw_path']).read_text(encoding='utf-8').splitlines()]
                    message = next(e for e in events if e['type'] == 'response_item')
                    self.assertEqual(len(blocks), row['message_count'])
                    self.assertEqual(blocks[0][2], dt.datetime.fromisoformat(message['timestamp'].replace('Z', '+00:00')))
                    self.assertEqual(row['started_at'], '2026-10-14T10:00:00+08:00')
                    self.assertEqual(row['source_size'], Path(row['raw_path']).stat().st_size)


def case_test(case):
    def test(self):
        self.check_case(case)
    return test


for case in CASES:
    setattr(CompleteFixtureTests, 'test_full_read_' + case.replace('-', '_'), case_test(case))

if __name__ == '__main__':
    unittest.main(verbosity=2)
