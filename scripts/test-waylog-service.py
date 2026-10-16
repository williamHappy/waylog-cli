#!/usr/bin/env python3
"""Offline integration tests: remotes are temporary local bare repositories."""
import importlib.util
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("service", Path(__file__).with_name("waylog-service.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def git(path, *args):
    return subprocess.check_output(["git", "-C", str(path), *args], stderr=subprocess.STDOUT, text=True).strip()


class RunnerOutputTests(unittest.TestCase):
    def test_machine_output_preserves_paths_and_excludes_warnings(self):
        code = "import sys; sys.stdout.write(' leading name.md\\0next.md\\0'); sys.stderr.write('warning: LF will be replaced by CRLF\\n')"
        diagnostics = io.StringIO()
        with contextlib.redirect_stderr(diagnostics):
            status, output = module.run([module.sys.executable, "-c", code], machine_output=True)
        self.assertEqual(status, 0)
        self.assertEqual(output.split("\0"), [" leading name.md", "next.md", ""])
        self.assertIn("LF will be replaced", diagnostics.getvalue())

    def test_machine_output_failure_retains_error_diagnostics(self):
        code = "import sys; sys.stderr.write('fatal: missing repository\\n'); sys.exit(128)"
        with self.assertRaisesRegex(module.ServiceError, "fatal: missing repository"):
            module.run([module.sys.executable, "-c", code], machine_output=True)

    def test_ordinary_output_keeps_push_rejection_text(self):
        code = "import sys; sys.stderr.write('remote: error: GH001: Large files detected\\n'); sys.exit(1)"
        status, output = module.run([module.sys.executable, "-c", code], check=False)
        self.assertEqual(status, 1)
        self.assertIn("GH001", output)


class SyncTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="waylog-service-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.source = self.root / "archive"
        self.source.mkdir()
        (self.source / "sessions").mkdir()
        (self.source / "sessions/session.md").write_text("first\n")
        (self.source / "manifest.json").write_text("{}")
        subprocess.check_call(["git", "init", "--bare", "--initial-branch=main", str(self.remote)], stdout=subprocess.DEVNULL)
        seed = self.root / "seed"
        subprocess.check_call(["git", "clone", str(self.remote), str(seed)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        git(seed, "config", "user.name", "Test")
        git(seed, "config", "user.email", "test@example.invalid")
        (seed / "README.md").write_text("seed\n")
        git(seed, "add", ".")
        git(seed, "commit", "-m", "seed")
        git(seed, "push", "origin", "main")
        self.other = seed
        import argparse
        args = argparse.Namespace(archive_dir=str(self.source), state_dir=str(self.root / "state"),
                                  repo_path="ai/raw/wk-me", remote=str(self.remote), branch="main",
                                  interval=3600, timeout=15, git_name="Test", git_email="test@example.invalid",
                                  waylog="/unused", no_browser=False)
        self.service = module.Service(args)
        self.clock = module.time.time()
        patcher = mock.patch.object(module.time, "time", side_effect=lambda: self.clock)
        patcher.start()
        self.addCleanup(patcher.stop)
        SyncTests.install_sync_clock(self)

    def install_sync_clock(self):
        actual_sync = self.service.sync
        def hourly_sync(*args, **kwargs):
            self.clock += 3601
            return actual_sync(*args, **kwargs)
        self.service.sync = hourly_sync

    def remote_head(self):
        return git(self.remote, "rev-parse", "main")

    def record_git_calls(self):
        calls = []
        original = self.service.git
        def recording_git(*args, **kwargs):
            calls.append(args)
            return original(*args, **kwargs)
        self.service.git = recording_git
        return calls

    def assert_pull_without_upload(self, calls):
        self.assertEqual(sum(args[0] == "fetch" for args in calls), 1)
        self.assertEqual(sum(args[0] == "rebase" for args in calls), 1)
        self.assertFalse(any(args[0] == "push" or args[:2] == ("lfs", "push") for args in calls))
        self.assertEqual(self.service.pending(), 0)
        status = json.loads((self.service.state / "status.json").read_text())
        self.assertEqual(status["sync_action"], "pull-only")
        self.assertIn("last_pull", status)

    def publish_other_machine_archive(self):
        git(self.other, "pull", "--ff-only")
        path = self.other / "ai/raw/other/sessions/company.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("company work\n")
        git(self.other, "add", "ai/raw/other")
        git(self.other, "commit", "-m", "other machine archive")
        git(self.other, "push")
        return path.relative_to(self.other)

    def test_sync_no_change_and_deletion_preservation(self):
        self.service.sync()
        initial = self.remote_head()
        local = git(self.service.repo, "rev-parse", "HEAD")
        calls = self.record_git_calls()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertEqual(initial, self.remote_head())
        self.assertEqual(local, git(self.service.repo, "rev-parse", "HEAD"))
        calls.clear()
        (self.source / "sessions/session.md").unlink()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertTrue((self.service.repo / "ai/raw/wk-me/sessions/session.md").exists())

    def test_idle_machine_receives_other_machine_archive_without_committing(self):
        self.service.sync()
        local = git(self.service.repo, "rev-parse", "HEAD")
        other_file = self.publish_other_machine_archive()
        remote = self.remote_head()
        self.assertNotEqual(local, remote)
        calls = self.record_git_calls()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertEqual((self.service.repo / other_file).read_text(), "company work\n")
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), remote)
        self.assertEqual(self.remote_head(), remote)

    def test_empty_isolated_archive_still_receives_other_devices(self):
        (self.source / "sessions/session.md").unlink()
        (self.source / "manifest.json").unlink()
        other_file = self.publish_other_machine_archive()
        remote = self.remote_head()
        calls = self.record_git_calls()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertEqual((self.service.repo / other_file).read_text(), "company work\n")
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), remote)
        self.assertEqual(self.remote_head(), remote)

    def test_failed_push_retained_and_retried_without_new_changes(self):
        self.service.sync()
        (self.source / "sessions/session.md").write_text("second\n")
        original_git = self.service.git
        def fail_push(*args, **kwargs):
            if args[0] == "push":
                return 1, "simulated network failure"
            return original_git(*args, **kwargs)
        self.service.git = fail_push
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertEqual(self.service.pending(), 1)
        self.service.git = original_git
        self.service.sync()
        self.assertEqual(self.service.pending(), 0)
        self.assertEqual(git(self.remote, "show", "main:ai/raw/wk-me/sessions/session.md"), "second")

    def test_other_machine_directory_rebases(self):
        self.service.sync()
        git(self.other, "pull", "--ff-only")
        (self.other / "ai/raw/other").mkdir(parents=True)
        (self.other / "ai/raw/other/file.md").write_text("other")
        git(self.other, "add", ".")
        git(self.other, "commit", "-m", "other machine")
        git(self.other, "push")
        (self.source / "sessions/session.md").write_text("second\n")
        self.service.sync()
        self.assertEqual(git(self.remote, "show", "main:ai/raw/other/file.md"), "other")

    def test_conflict_blocks_and_preserves_commit(self):
        self.service.sync()
        git(self.other, "pull", "--ff-only")
        (self.other / "ai/raw/wk-me/sessions/session.md").write_text("remote edit\n")
        git(self.other, "add", ".")
        git(self.other, "commit", "-m", "conflicting edit")
        git(self.other, "push")
        remote = self.remote_head()
        (self.source / "sessions/session.md").write_text("local edit\n")
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertTrue((self.service.state / "BLOCKED").exists())
        self.assertEqual(self.remote_head(), remote)
        self.assertEqual(self.service.pending(), 1)
        self.assertFalse(git(self.service.repo, "status", "--porcelain"))
        with self.assertRaises(module.ServiceError):
            self.service.sync()

    def test_dry_run_does_not_create_state(self):
        self.service.sync(dry_run=True)
        self.assertFalse(self.service.state.exists())

    def test_unicode_archive_pending_commit_can_be_retried(self):
        filename = "sessions/中文 会话.md"
        (self.source / filename).write_text("中文\n", encoding="utf-8")
        self.service.sync()
        (self.source / filename).write_text("中文更新\n", encoding="utf-8")
        original_git = self.service.git
        def fail_push(*args, **kwargs):
            return (1, "network failure") if args[0] == "push" else original_git(*args, **kwargs)
        self.service.git = fail_push
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.service.git = original_git
        self.service.sync()
        self.assertEqual(self.service.pending(), 0)

    def test_windows_lock_branch_releases_byte_lock(self):
        from types import SimpleNamespace
        calls = []
        crt = SimpleNamespace(LK_NBLCK=2, LK_UNLCK=0,
                              locking=lambda fd, mode, size: calls.append((mode, size)))
        with mock.patch.object(module, "os", SimpleNamespace(name="nt")), \
                mock.patch.object(module, "msvcrt", crt, create=True):
            with self.service.lock("test"):
                self.assertEqual(calls, [(2, 1)])
        self.assertEqual(calls, [(2, 1), (0, 1)])

    def test_windows_action_arguments_quote_paths_without_registering(self):
        self.service.args.git_name = "中文 用户"
        command = self.service.command_arguments("watch-worker")
        quoted = subprocess.list2cmdline(command[1:])
        self.assertIn('"中文 用户"', quoted)
        self.assertTrue(quoted.endswith("watch-worker"))
        self.assertIn("--isolated", command)
        self.assertNotIn("--git-repo", command)
        self.assertFalse(self.service.state.exists())

    def test_lfs_file_upload_to_local_remote(self):
        (self.other / ".gitattributes").write_text("ai/raw/wk-me/sessions/*.raw.jsonl filter=lfs diff=lfs merge=lfs -text\n")
        git(self.other, "add", ".gitattributes")
        git(self.other, "commit", "-m", "LFS rule")
        git(self.other, "push")
        (self.source / "sessions/session.raw.jsonl").write_text('{"message":"test"}\n')
        self.service.sync()
        pointer = git(self.remote, "show", "main:ai/raw/wk-me/sessions/session.raw.jsonl")
        self.assertTrue(pointer.startswith("version https://git-lfs.github.com/spec/v1"))
        self.assertTrue(list((self.remote / "lfs/objects").rglob("*")))

    def test_launchagent_configuration_without_launching(self):
        home = self.root / "fake-home"
        self.service.state.mkdir()
        calls = []
        def fake_run(args, **kwargs):
            calls.append(args)
            if args[:3] == ["/unused", "watch", "--help"]:
                return 0, "--instance-lock-file --service-log-file --archive-lock-file --git-dir"
            return (1, "not loaded") if args[1] == "print" else (0, "")
        with mock.patch.object(module.sys, "platform", "darwin"), \
                mock.patch.object(module.Path, "home", return_value=home), \
                mock.patch.object(module, "run", side_effect=fake_run):
            self.service.launch("start")
        import plistlib
        sync = plistlib.loads((home / "Library/LaunchAgents/local.waylog-service.sync.plist").read_bytes())
        watch = plistlib.loads((home / "Library/LaunchAgents/local.waylog-service.watch.plist").read_bytes())
        self.assertEqual(sync["StartCalendarInterval"], {"Minute": 0})
        self.assertTrue(sync["RunAtLoad"])
        self.assertTrue(watch["KeepAlive"])
        self.assertEqual(sync["ProgramArguments"][-1], "sync")
        self.assertEqual(watch["ProgramArguments"][0], "/unused")
        self.assertIn("watch", watch["ProgramArguments"])
        self.assertNotIn(str(Path(module.__file__).resolve()), watch["ProgramArguments"])
        self.assertEqual(watch["ExitTimeOut"], 15)
        self.assertEqual(watch["StandardOutPath"], str(self.service.state / "logs/watch-launch.log"))
        self.assertEqual(watch["ProgramArguments"][-4:], ["--instance-lock-file", str(self.service.state / "watch.lock"),
                                                        "--service-log-file", str(self.service.state / "logs/watch-output.log")])
        self.assertEqual(sum(args[1] == "bootstrap" for args in calls), 2)

    def test_malformed_index_not_committed(self):
        self.service.sync()
        before = self.remote_head()
        (self.source / "indexes").mkdir()
        (self.source / "indexes/sessions.jsonl").write_text("{broken")
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertEqual(before, self.remote_head())

    def test_symlink_and_dirty_worktree_rejected(self):
        self.service.sync()
        (self.source / "sessions/link").symlink_to(self.source / "manifest.json")
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        (self.source / "sessions/link").unlink()
        (self.service.repo / "README.md").write_text("manual change")
        with self.assertRaises(module.ServiceError):
            self.service.sync()


class DirectSyncTests(unittest.TestCase):
    remote_head = SyncTests.remote_head
    record_git_calls = SyncTests.record_git_calls
    assert_pull_without_upload = SyncTests.assert_pull_without_upload
    publish_other_machine_archive = SyncTests.publish_other_machine_archive

    def setUp(self):
        SyncTests.setUp(self)
        checkout = self.root / "existing-repo"
        subprocess.check_call(["git", "clone", str(self.remote), str(checkout)],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.source = checkout / "ai/raw/wk-me"
        self.source.mkdir(parents=True)
        (self.source / "sessions").mkdir()
        (self.source / "sessions/session.md").write_text("first\n")
        (self.source / "manifest.json").write_text("{}")
        self.service.args.git_repo = str(checkout)
        self.service.args.archive_dir = str(self.source)
        self.service = module.Service(self.service.args)
        SyncTests.install_sync_clock(self)

    def test_direct_failed_push_retained(self):
        SyncTests.test_failed_push_retained_and_retried_without_new_changes(self)

    def test_direct_other_machine_rebases(self):
        SyncTests.test_other_machine_directory_rebases(self)

    def prepare_archive_merge(self):
        git(self.service.repo, "config", "user.name", "Test")
        git(self.service.repo, "config", "user.email", "test@example.invalid")
        git(self.service.repo, "add", "ai/raw/wk-me")
        git(self.service.repo, "commit", "-m", "archive\n\nWaylog-Service-Archive: ai/raw/wk-me")
        other = self.other / "ai/raw/wk-jzx"
        other.mkdir(parents=True)
        (other / "example.md").write_text("Windows archive\n")
        git(self.other, "add", "ai/raw/wk-jzx")
        git(self.other, "commit", "-m", "Windows archive")
        git(self.other, "push")
        git(self.service.repo, "fetch")
        git(self.service.repo, "merge", "--no-edit", "origin/main")

    def test_published_other_device_merge_is_accepted(self):
        self.prepare_archive_merge()
        self.service.verify_direct_pending()

    def test_sync_preserves_accepted_merge_and_publishes_it(self):
        self.prepare_archive_merge()
        merge = git(self.service.repo, "rev-parse", "HEAD")
        self.service.sync()
        self.assertEqual(git(self.service.repo, "merge-base", "--is-ancestor", merge, "HEAD"), "")
        self.assertEqual(git(self.remote, "rev-parse", "main"), git(self.service.repo, "rev-parse", "HEAD"))

    def test_merge_with_added_own_archive_content_is_rejected(self):
        self.prepare_archive_merge()
        (self.source / "sessions/session.md").write_text("manual merge edit\n")
        git(self.service.repo, "add", "ai/raw/wk-me")
        git(self.service.repo, "commit", "--amend", "--no-edit")
        with self.assertRaisesRegex(module.ServiceError, "Merge includes"):
            self.service.verify_direct_pending()

    def test_merge_with_added_other_device_content_is_rejected(self):
        self.prepare_archive_merge()
        (self.service.repo / "ai/raw/wk-jzx/example.md").write_text("manual remote edit\n")
        git(self.service.repo, "add", "ai/raw/wk-jzx")
        git(self.service.repo, "commit", "--amend", "--no-edit")
        with self.assertRaisesRegex(module.ServiceError, "Merge includes"):
            self.service.verify_direct_pending()

    def test_direct_conflict_preserves_commit(self):
        SyncTests.test_conflict_blocks_and_preserves_commit(self)

    def test_direct_unicode_retry(self):
        SyncTests.test_unicode_archive_pending_commit_can_be_retried(self)

    def test_direct_malformed_index(self):
        SyncTests.test_malformed_index_not_committed(self)

    def test_direct_lfs(self):
        (self.other / ".gitattributes").write_text("ai/raw/wk-me/sessions/*.raw.jsonl filter=lfs diff=lfs merge=lfs -text\n")
        git(self.other, "add", ".gitattributes")
        git(self.other, "commit", "-m", "LFS rule")
        git(self.other, "push")
        git(self.service.repo, "pull", "--ff-only")
        (self.source / "sessions/session.raw.jsonl").write_text('{"message":"test"}\n')
        self.service.sync()
        pointer = git(self.remote, "show", "main:ai/raw/wk-me/sessions/session.raw.jsonl")
        self.assertTrue(pointer.startswith("version https://git-lfs.github.com/spec/v1"))
        self.assertTrue(list((self.remote / "lfs/objects").rglob("*")))

    def test_direct_no_clone_and_no_change_pulls_without_upload(self):
        with mock.patch.object(self.service, "initialize_repo", side_effect=AssertionError("must not clone")):
            self.service.sync()
        self.assertFalse((self.service.state / "repo").exists())
        head = self.remote_head()
        calls = self.record_git_calls()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertEqual(self.remote_head(), head)
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), head)

    def test_direct_idle_machine_receives_other_machine_archive(self):
        SyncTests.test_idle_machine_receives_other_machine_archive_without_committing(self)

    def test_direct_idle_pull_materializes_other_device_lfs_data_without_upload(self):
        self.service.sync()
        git(self.other, "pull", "--ff-only")
        git(self.other, "lfs", "install", "--local")
        (self.other / ".gitattributes").write_text(
            "ai/raw/other/sessions/*.raw.jsonl filter=lfs diff=lfs merge=lfs -text\n")
        relative = Path("ai/raw/other/sessions/company.raw.jsonl")
        content = '{"message":"company work"}\n'
        (self.other / relative).parent.mkdir(parents=True, exist_ok=True)
        (self.other / relative).write_text(content)
        git(self.other, "add", ".gitattributes", str(relative))
        git(self.other, "commit", "-m", "other device LFS archive")
        git(self.other, "push")
        remote = self.remote_head()
        self.assertTrue(git(self.remote, "show", f"main:{relative.as_posix()}").startswith(
            "version https://git-lfs.github.com/spec/v1"))
        calls = self.record_git_calls()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertEqual((self.service.repo / relative).read_text(), content)
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), remote)
        self.assertEqual(self.remote_head(), remote)

    def test_direct_empty_archive_without_manifest_pulls_other_devices(self):
        (self.source / "sessions/session.md").unlink()
        (self.source / "manifest.json").unlink()
        other_file = self.publish_other_machine_archive()
        remote = self.remote_head()
        calls = self.record_git_calls()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertEqual((self.service.repo / other_file).read_text(), "company work\n")
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), remote)
        self.assertEqual(self.remote_head(), remote)

    def test_direct_missing_own_archive_pulls_other_devices(self):
        (self.source / "sessions/session.md").unlink()
        (self.source / "manifest.json").unlink()
        (self.source / "sessions").rmdir()
        self.source.rmdir()
        other_file = self.publish_other_machine_archive()
        remote = self.remote_head()
        calls = self.record_git_calls()
        self.service.sync()
        self.assert_pull_without_upload(calls)
        self.assertFalse(self.source.exists())
        self.assertEqual((self.service.repo / other_file).read_text(), "company work\n")
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), remote)
        self.assertEqual(self.remote_head(), remote)

    def test_direct_dirty_other_directory_is_preserved(self):
        (self.service.repo / "README.md").write_text("manual edit")
        head = self.remote_head()
        with self.assertRaisesRegex(module.ServiceError, "Other directories"):
            self.service.sync()
        self.assertEqual((self.service.repo / "README.md").read_text(), "manual edit")
        self.assertEqual(self.remote_head(), head)
        self.assertFalse(git(self.service.repo, "diff", "--cached", "--name-only"))

    def test_direct_staged_changes_are_not_committed(self):
        git(self.service.repo, "add", "ai/raw/wk-me")
        staged = git(self.service.repo, "diff", "--cached", "--name-only")
        with self.assertRaisesRegex(module.ServiceError, "staged changes"):
            self.service.sync()
        self.assertEqual(git(self.service.repo, "diff", "--cached", "--name-only"), staged)

    def test_direct_manual_unpushed_commit_is_not_pushed(self):
        git(self.service.repo, "config", "user.name", "Test")
        git(self.service.repo, "config", "user.email", "test@example.invalid")
        (self.service.repo / "README.md").write_text("manual commit")
        git(self.service.repo, "add", "README.md")
        git(self.service.repo, "commit", "-m", "manual")
        head = self.remote_head()
        with self.assertRaisesRegex(module.ServiceError, "not created by this archive service"):
            self.service.sync()
        self.assertEqual(self.remote_head(), head)

    def test_direct_manual_archive_commit_is_not_pushed(self):
        git(self.service.repo, "config", "user.name", "Test")
        git(self.service.repo, "config", "user.email", "test@example.invalid")
        git(self.service.repo, "add", "ai/raw/wk-me")
        git(self.service.repo, "commit", "-m", "manual archive commit")
        head = self.remote_head()
        with self.assertRaisesRegex(module.ServiceError, "not created by this archive service"):
            self.service.sync()
        self.assertEqual(self.remote_head(), head)

    def test_direct_other_untracked_file_is_not_staged(self):
        (self.service.repo / "personal-notes.txt").write_text("private")
        with self.assertRaisesRegex(module.ServiceError, "Other directories"):
            self.service.sync()
        self.assertFalse(git(self.service.repo, "diff", "--cached", "--name-only"))

    def test_direct_https_origin_is_preserved_and_network_uses_ssh(self):
        https = "https://github.com/williamHappy/i-raw-data.git"
        ssh = "git@github.com:williamHappy/i-raw-data.git"
        git(self.service.repo, "remote", "set-url", "origin", https)
        self.service.args.remote = ssh
        self.service.check_repo(allow_archive=True)
        with mock.patch.object(module, "run", return_value=(0, "")) as mocked:
            self.service.git("fetch", "origin", "main")
            self.service.git("rebase", "origin/main")
        for call in mocked.call_args_list:
            command = call.args[0]
            if "--get-all" in command:
                continue
            self.assertIn(f"remote.origin.url={ssh}", command)
            self.assertIn(f"remote.origin.pushurl={ssh}", command)
        self.assertEqual(git(self.service.repo, "remote", "get-url", "origin"), https)

    def test_fetch_resolves_saved_https_origin_to_configured_ssh_without_network(self):
        https = "https://github.com/williamHappy/i-raw-data.git"
        ssh = "git@github.com:williamHappy/i-raw-data.git"
        git(self.service.repo, "remote", "set-url", "origin", https)
        self.service.args.remote = ssh
        original_run = module.run
        resolved = []

        def inspect_fetch(command, **kwargs):
            if "fetch" in command:
                prefix = command[:command.index("fetch")]
                code, url = original_run(prefix + ["remote", "get-url", "origin"], **kwargs)
                resolved.append(url.strip())
                return code, ""
            return original_run(command, **kwargs)

        with mock.patch.object(module, "run", side_effect=inspect_fetch):
            self.service.git("fetch", "origin", "main")
        self.assertEqual(resolved, [ssh])
        self.assertEqual(git(self.service.repo, "remote", "get-url", "origin"), https)

    def test_direct_unknown_archive_file_is_not_committed(self):
        (self.source / "personal-note.txt").write_text("do not publish")
        head = git(self.service.repo, "rev-parse", "HEAD")
        with self.assertRaisesRegex(module.ServiceError, "supported archive files"):
            self.service.sync()
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), head)
        self.assertFalse(git(self.service.repo, "diff", "--cached", "--name-only"))

    def test_direct_dry_run_creates_no_state_or_commit(self):
        before = git(self.service.repo, "rev-parse", "HEAD")
        self.service.sync(dry_run=True)
        self.assertFalse(self.service.state.exists())
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), before)
        self.assertFalse(git(self.service.repo, "diff", "--cached", "--name-only"))

    def test_direct_scheduled_arguments_preserve_existing_repository_mode(self):
        for action in ("watch-worker", "sync-worker"):
            command = self.service.command_arguments(action)
            self.assertEqual(command[command.index("--git-repo") + 1], str(self.service.repo))
            self.assertNotIn("--isolated", command)
            self.assertEqual(command[-1], action)
        self.assertFalse(self.service.state.exists())

    def test_direct_archive_lock_prevents_git_mutation(self):
        self.service.args.timeout = 0.05
        with self.service.lock("archive-access"):
            with self.assertRaisesRegex(module.ServiceError, "Archive is busy"):
                self.service.sync()
        self.assertFalse(git(self.service.repo, "diff", "--cached", "--name-only"))

    def test_direct_tracked_deletion_is_rejected(self):
        self.service.sync()
        (self.source / "sessions/session.md").unlink()
        head = self.remote_head()
        with self.assertRaisesRegex(module.ServiceError, "deletions"):
            self.service.sync()
        self.assertEqual(self.remote_head(), head)

    def test_direct_watch_uses_native_watch_with_cycle_coordination(self):
        from types import SimpleNamespace
        git_dir = git(self.service.repo, "rev-parse", "--absolute-git-dir")
        def fake_help(args, **kwargs):
            if args[0] == "/unused":
                self.assertEqual(args, ["/unused", "watch", "--help"])
                return 0, "--archive-lock-file <PATH> --git-dir <PATH>"
            self.assertEqual(args[-2:], ["rev-parse", "--absolute-git-dir"])
            return 0, git_dir
        child = SimpleNamespace(stdout=io.StringIO("fake native watch\n"), poll=lambda: 0, wait=lambda: 0)
        previous = {kind: module.signal.getsignal(kind) for kind in (module.signal.SIGTERM, module.signal.SIGINT)}
        def fake_spawn(command, **kwargs):
            # The supervisor does not hold the archive lock for the child's lifetime.
            # Rust watch acquires it only for full collection cycles.
            with self.service.lock("archive-access"):
                pass
            return child
        with mock.patch.object(module, "run", side_effect=fake_help), \
                mock.patch.object(module.subprocess, "Popen", side_effect=fake_spawn) as spawned:
            self.assertEqual(self.service.watch(), 0)
        command = spawned.call_args.args[0]
        self.assertEqual(command[:3], ["/unused", "--verbose", "watch"])
        self.assertNotIn("export", command)
        self.assertEqual(command[command.index("--archive-lock-file") + 1],
                         str(self.service.state / "archive-access.lock"))
        self.assertEqual(command[command.index("--git-dir") + 1],
                         git(self.service.repo, "rev-parse", "--absolute-git-dir"))
        self.assertEqual(spawned.call_count, 1)
        self.assertEqual(previous, {kind: module.signal.getsignal(kind) for kind in previous})

    def test_direct_watch_rejects_old_binary_before_starting_child(self):
        with mock.patch.object(module, "run", return_value=(0, "Options: --archive-dir")), \
                mock.patch.object(module.subprocess, "Popen", side_effect=AssertionError("must not launch watch")):
            with self.assertRaisesRegex(module.ServiceError, "lacks native watch/Git coordination"):
                self.service.watch()

    def test_native_watch_keeps_browser_option(self):
        self.service.args.no_browser = True
        real_run = module.run
        def fake_help(args, **kwargs):
            return (0, "--archive-lock-file --git-dir") if args[0] == "/unused" else real_run(args, **kwargs)
        with mock.patch.object(module, "run", side_effect=fake_help):
            self.assertIn("--no-browser", self.service.watch_command())

    def test_dedicated_watch_does_not_require_direct_coordination_flags(self):
        self.service.direct = False
        with mock.patch.object(module, "run", side_effect=AssertionError("no Git or help process needed")):
            command = self.service.watch_command()
        self.assertEqual(command[1:3], ["--verbose", "watch"])
        self.assertNotIn("--archive-lock-file", command)

    def test_macos_native_launch_requires_new_binary_without_spawning_collector(self):
        with mock.patch.object(module, "run", return_value=(0, "--archive-lock-file --git-dir")), \
                mock.patch.object(module.subprocess, "Popen", side_effect=AssertionError("no collector")):
            with self.assertRaisesRegex(module.ServiceError, "direct macOS background"):
                self.service.macos_watch_command()

    def test_macos_native_launch_keeps_browser_cycle_and_git_coordination(self):
        self.service.args.no_browser = True
        self.service.watch_interval = 45
        real_run = module.run
        def help_or_git(args, **kwargs):
            if args[0] == "/unused":
                return 0, "--archive-lock-file --git-dir --interval-seconds --instance-lock-file --service-log-file"
            return real_run(args, **kwargs)
        with mock.patch.object(module, "run", side_effect=help_or_git):
            command = self.service.macos_watch_command()
        self.assertEqual(command[0], "/unused")
        self.assertIn("--no-browser", command)
        self.assertIn("--archive-lock-file", command)
        self.assertIn("--git-dir", command)
        self.assertEqual(command[command.index("--interval-seconds") + 1], "45")
        self.assertEqual(command[command.index("--instance-lock-file") + 1], str(self.service.state / "watch.lock"))


class RetryPolicyTests(unittest.TestCase):
    remote_head = SyncTests.remote_head

    def setUp(self):
        DirectSyncTests.setUp(self)
        module.Service.sync(self.service)
        self.clock += 3601
        (self.source / "sessions/session.md").write_text("retry data\n")
        self.push_calls = 0
        original = self.service.git
        self.failure_message = "simulated network timeout"
        def fail_push(*args, **kwargs):
            if args[0] == "push":
                self.push_calls += 1
                return 1, self.failure_message
            return original(*args, **kwargs)
        self.service.git = fail_push

    def fail_round(self):
        with self.assertRaises(module.ServiceError):
            module.Service.sync(self.service)

    def test_minute_ticks_for_one_day_make_only_three_failed_network_rounds(self):
        self.fail_round()
        with contextlib.redirect_stdout(io.StringIO()):
            for minute in range(1440):
                self.clock += 60
                try:
                    module.Service.sync(self.service)
                except module.ServiceError:
                    pass
        self.assertEqual(self.push_calls, 3)
        self.assertEqual(self.service.network_state()["consecutive_failures"], 3)
        self.assertTrue(self.service.network_state()["paused"])

    def test_failure_backoff_then_circuit_pause(self):
        self.fail_round()
        self.assertEqual(self.service.network_state()["next_retry_epoch"], self.clock + 3600)
        self.clock += 3601
        self.fail_round()
        self.assertEqual(self.service.network_state()["next_retry_epoch"], self.clock + 7200)
        self.clock += 7201
        self.fail_round()
        self.assertTrue(self.service.network_state()["paused"])

    def test_new_process_and_manual_triggers_cannot_bypass_cooldown(self):
        self.fail_round()
        fresh = module.Service(self.service.args)
        with mock.patch.object(module, "run", side_effect=AssertionError("must not spawn any Git process")):
            for repeat in range(5):
                module.Service.sync(fresh)
        self.assertEqual(self.push_calls, 1)

    def test_non_fast_forward_retries_are_one_bounded_round(self):
        self.failure_message = "!\tHEAD:refs/heads/main\t[rejected] (non-fast-forward)"
        with mock.patch.object(module.CANCELLED, 'wait', return_value=False):
            self.fail_round()
        self.assertEqual(self.push_calls, 3)
        self.assertEqual(self.service.network_state()['consecutive_failures'], 1)

    def test_github_size_rejection_pauses_after_first_attempt(self):
        self.failure_message = "remote: error: GH001: Large files detected. pre-receive hook declined"
        self.fail_round()
        state = self.service.network_state()
        self.assertTrue(state["paused"])
        self.assertEqual(state["consecutive_failures"], 1)
        self.assertIn("GH001", state["pause_reason"])
        self.clock += 86400
        fresh = module.Service(self.service.args)
        with mock.patch.object(module, "run", side_effect=AssertionError("must not retransmit rejected history")):
            module.Service.sync(fresh)
        self.assertEqual(self.push_calls, 1)
        self.service.resume_sync()
        state = self.service.network_state()
        self.assertFalse(state["paused"])
        self.assertNotIn("pause_reason", state)

    def test_resume_does_not_connect_or_bypass_minimum_interval(self):
        for count in range(3):
            if count:
                self.clock = self.service.network_state()["next_retry_epoch"] + 1
            self.fail_round()
        with mock.patch.object(module, "run", side_effect=AssertionError("must not connect")):
            self.service.resume_sync()
            module.Service.sync(self.service)
        data = self.service.network_state()
        self.assertFalse(data["paused"])
        self.assertEqual(data["consecutive_failures"], 0)
        self.assertEqual(data["next_retry_epoch"], self.clock + 3600)

    def test_interrupted_attempt_is_reserved_before_process_spawn(self):
        self.service.begin_network()
        data = self.service.network_state()
        self.assertTrue(data["in_flight"])
        self.assertEqual(data["consecutive_failures"], 1)
        fresh = module.Service(self.service.args)
        self.assertFalse(fresh.network_allowed())
        self.clock += 3601
        fresh.begin_network()
        self.assertEqual(fresh.network_state()["consecutive_failures"], 2)

    def test_lfs_retry_settings_and_duplicate_upload_hook(self):
        fresh = module.Service(self.service.args)
        fresh._lfs_ready_for_push = True
        with mock.patch.object(module, "run", return_value=(0, "")) as mocked:
            fresh.git("push", "origin", "main")
        command = mocked.call_args.args[0]
        self.assertIn("lfs.transfer.maxretries=1", command)
        self.assertIn("lfs.ssh.retries=1", command)
        self.assertIn("lfs.transfer.maxverifies=1", command)
        self.assertEqual(mocked.call_args.kwargs["env"]["GIT_LFS_SKIP_PUSH"], "1")

    def test_corrupt_retry_state_refuses_network(self):
        (self.service.state / "network-state.json").write_text("{broken")
        with mock.patch.object(module, "run", side_effect=AssertionError("must not connect")):
            with self.assertRaisesRegex(module.ServiceError, "Invalid network-state"):
                module.Service.sync(self.service)


class AutomaticLfsRecoveryTests(unittest.TestCase):
    def setUp(self):
        DirectSyncTests.setUp(self)
        self.service.sync()
        self.remote_before = git(self.remote, "rev-parse", "main")
        self.raw = self.source / "sessions/large.raw.jsonl"
        self.payload = '{"archive": "offline recovery test"}\n'
        self.raw.write_text(self.payload)
        self.push_calls = 0
        self.calls = []
        original = self.service.git
        self.rejected_path = "ai/raw/wk-me/sessions/large.raw.jsonl"
        self.fail_migration = False
        def fake_failure(*args, **kwargs):
            self.calls.append(args)
            if args[0] == "push":
                self.push_calls += 1
                if self.push_calls == 1:
                    git(self.service.repo, "branch", "local-backup", "HEAD")
                    return 1, ("remote: error: File " + self.rejected_path +
                               " is 101.61 MB; this exceeds GitHub's file size limit\n"
                               "remote: error: GH001: Large files detected.")
            if self.fail_migration and args[:3] == ("lfs", "migrate", "import"):
                raise module.ServiceError("simulated migration failure")
            return original(*args, **kwargs)
        self.service.git = fake_failure

    def test_migration_adds_rules_preserves_backup_and_uploads_next_cycle(self):
        hook = self.service.repo / ".git/hooks/pre-push"
        hook.write_text("#!/bin/sh\nexit 0\n")
        hook.chmod(0o755)
        original_hook = hook.read_bytes()
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        status = json.loads((self.service.state / "status.json").read_text())
        self.assertEqual(status["lfs_recovery"], "ready-for-next-cycle")
        self.assertEqual(self.push_calls, 1)
        self.assertEqual(git(self.remote, "rev-parse", "main"), self.remote_before)
        self.assertEqual(git(self.service.repo, "rev-parse", "origin/main"), self.remote_before)
        self.assertNotEqual(status["lfs_old_head"], status["lfs_new_head"])
        self.assertEqual(git(self.service.repo, "rev-parse", "local-backup"), status["lfs_old_head"])
        self.assertIn(status["lfs_old_head"], git(self.service.repo, "bundle", "list-heads", status["lfs_backup"]))
        self.assertEqual(self.raw.read_text(), self.payload)
        self.assertEqual(hook.read_bytes(), original_hook)
        self.assertTrue(git(self.service.repo, "show", "HEAD:" + self.rejected_path).startswith(
            "version https://git-lfs.github.com/spec/v1"))
        self.assertIn(self.rejected_path, (self.service.repo / ".gitattributes").read_text())
        self.service.verify_direct_pending()
        self.assertFalse(self.service.network_state()["paused"])
        self.assertEqual(self.service.network_state()["consecutive_failures"], 1)
        self.service.sync()
        self.assertEqual(self.push_calls, 2)
        self.assertEqual(self.service.pending(), 0)
        self.assertIn("oid sha256:", git(self.remote, "show", "main:" + self.rejected_path))

    def test_successful_repair_still_honors_configured_failure_limit(self):
        self.service.retry_max_failures = 1
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertEqual(json.loads((self.service.state / "status.json").read_text())["lfs_recovery"],
                         "ready-for-next-cycle")
        self.assertTrue(self.service.network_state()["paused"])
        self.service.sync()
        self.assertEqual(self.push_calls, 1)
        self.service.resume_sync()
        self.service.sync()
        self.assertEqual(self.push_calls, 2)

    def test_existing_lfs_archive_remains_readable_after_recovery(self):
        older = self.source / "sessions/older.raw.jsonl"
        older.write_text('{"older": "already in LFS"}\n')
        (self.service.repo / ".gitattributes").write_text(
            "ai/raw/wk-me/sessions/older.raw.jsonl filter=lfs diff=lfs merge=lfs -text\n")
        git(self.service.repo, "config", "user.name", "Test")
        git(self.service.repo, "config", "user.email", "test@example.invalid")
        git(self.service.repo, "lfs", "install", "--local", "--skip-repo")
        git(self.service.repo, "add", ".gitattributes", "ai/raw/wk-me/sessions/older.raw.jsonl")
        git(self.service.repo, "commit", "-m", "published existing LFS archive")
        git(self.service.repo, "lfs", "push", "origin", "main")
        git(self.service.repo, "push", "origin", "main")
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertEqual(json.loads((self.service.state / "status.json").read_text())["lfs_recovery"],
                         "ready-for-next-cycle")
        self.assertEqual(older.read_text(), '{"older": "already in LFS"}\n')
        self.assertEqual(self.raw.read_text(), self.payload)

    def test_migration_failure_keeps_backup_and_pauses(self):
        self.fail_migration = True
        with self.assertRaisesRegex(module.ServiceError, "LFS recovery failed"):
            self.service.sync()
        status = json.loads((self.service.state / "status.json").read_text())
        self.assertEqual(status["lfs_recovery"], "failed")
        self.assertTrue(Path(status["lfs_backup"]).is_file())
        self.assertTrue((self.service.state / "BLOCKED").is_file())
        self.assertTrue(self.service.network_state()["paused"])
        self.assertEqual(self.raw.read_text(), self.payload)
        self.assertEqual(self.push_calls, 1)
        with mock.patch.object(module, "run", side_effect=AssertionError("must not retry failed migration")):
            module.Service.sync(self.service)

    def test_other_machine_path_is_not_migrated(self):
        self.rejected_path = "ai/raw/wk-jzx/sessions/foreign.raw.jsonl"
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertTrue(self.service.network_state()["paused"])
        self.assertFalse(any(args[:3] == ("lfs", "migrate", "import") for args in self.calls))
        self.assertFalse((self.service.repo / ".gitattributes").exists())
        self.assertEqual(self.raw.read_text(), self.payload)

    def test_tagged_pending_history_requires_manual_review(self):
        original = self.service.git
        def add_tag(*args, **kwargs):
            if args[0] == "push" and self.push_calls == 0:
                git(self.service.repo, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                    "tag", "-a", "keep-tag", "-m", "preserve independently published tag")
            return original(*args, **kwargs)
        self.service.git = add_tag
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        tag = git(self.service.repo, "rev-parse", "keep-tag")
        self.assertEqual(json.loads((self.service.state / "status.json").read_text())["lfs_recovery"],
                         "needs-tag-review")
        self.assertFalse(any(args[:3] == ("lfs", "migrate", "import") for args in self.calls))
        self.assertEqual(git(self.service.repo, "rev-parse", "keep-tag"), tag)
        self.assertTrue(self.service.network_state()["paused"])

    def test_other_machine_lfs_rule_cannot_bypass_commit_ownership(self):
        self.raw.unlink()
        git(self.service.repo, "config", "user.name", "Test")
        git(self.service.repo, "config", "user.email", "test@example.invalid")
        (self.service.repo / ".gitattributes").write_text(
            "ai/raw/wk-jzx/** filter=lfs diff=lfs merge=lfs -text\n")
        git(self.service.repo, "add", ".gitattributes")
        git(self.service.repo, "commit", "-m", "test\n\nWaylog-Service-Archive: ai/raw/wk-me")
        with self.assertRaisesRegex(module.ServiceError, "this device's LFS rules"):
            self.service.verify_direct_pending()


class IdlePullRetryTests(unittest.TestCase):
    remote_head = SyncTests.remote_head
    record_git_calls = SyncTests.record_git_calls
    assert_pull_without_upload = SyncTests.assert_pull_without_upload
    publish_other_machine_archive = SyncTests.publish_other_machine_archive

    def setUp(self):
        DirectSyncTests.setUp(self)
        module.Service.sync(self.service)
        self.clock += 3601

    def test_idle_fetch_failures_are_limited_without_commits_or_uploads(self):
        remote = self.remote_head()
        local = git(self.service.repo, "rev-parse", "HEAD")
        calls = self.record_git_calls()
        original_run = module.run
        def fail_fetch(args, **kwargs):
            if "fetch" in args:
                raise module.ServiceError("simulated idle fetch timeout")
            return original_run(args, **kwargs)
        with mock.patch.object(module, "run", side_effect=fail_fetch):
            with self.assertRaisesRegex(module.ServiceError, "idle fetch timeout"):
                module.Service.sync(self.service)
            self.assertEqual(self.service.network_state()["next_retry_epoch"], self.clock + 3600)
            fresh = module.Service(self.service.args)
            with mock.patch.object(module, "run", side_effect=AssertionError("cooldown must not run Git")):
                module.Service.sync(fresh)
            with contextlib.redirect_stdout(io.StringIO()):
                for minute in range(1440):
                    self.clock += 60
                    try:
                        module.Service.sync(self.service)
                    except module.ServiceError:
                        pass
        self.assertEqual(sum(args[0] == "fetch" for args in calls), 3)
        self.assertFalse(any(args[0] == "push" or args[:2] == ("lfs", "push") for args in calls))
        self.assertEqual(self.service.network_state()["consecutive_failures"], 3)
        self.assertTrue(self.service.network_state()["paused"])
        self.assertEqual(self.service.pending(), 0)
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), local)
        self.assertEqual(self.remote_head(), remote)

    def test_idle_fetch_retry_recovers_and_receives_other_device(self):
        original_run = module.run
        def fail_fetch(args, **kwargs):
            if "fetch" in args:
                raise module.ServiceError("simulated idle fetch timeout")
            return original_run(args, **kwargs)
        with mock.patch.object(module, "run", side_effect=fail_fetch):
            with self.assertRaises(module.ServiceError):
                module.Service.sync(self.service)
        self.assertEqual(self.service.pending(), 0)
        other_file = self.publish_other_machine_archive()
        remote = self.remote_head()
        self.clock += 3601
        calls = self.record_git_calls()
        module.Service.sync(self.service)
        self.assert_pull_without_upload(calls)
        self.assertEqual((self.service.repo / other_file).read_text(), "company work\n")
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), remote)
        self.assertEqual(self.remote_head(), remote)
        self.assertEqual(self.service.network_state()["consecutive_failures"], 0)


class ConfigurationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="waylog-config-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.config = self.root / "service.json"

    def load(self, data, overrides=None):
        self.config.write_text(json.dumps(data), encoding="utf-8-sig")
        return module.load_configuration(self.config, overrides)

    def test_platform_defaults(self):
        mac, windows = module.config_defaults(False), module.config_defaults(True)
        self.assertEqual((mac["repo_mode"], mac["pull_on_idle"]), ("full", True))
        self.assertEqual((windows["repo_mode"], windows["pull_on_idle"]), ("sparse", False))
        self.assertEqual(windows["repo_path"], "ai/raw/wk-jzx")
        self.assertEqual(mac["retry_max_failures"], 3)

    def test_line_comments_preserve_urls_windows_paths_and_escaped_quotes(self):
        raw = r'''{
          // 中文注释，默认 3600；不要影响真实配置
          "interval": 7200, // 当前两小时
          "remote": "https://github.com/example/repo.git", // URL 中的 // 保留
          "path": "C:\\Users\\test\\data", // 路径中的反斜杠保留
          "quote": "escaped \" // inside string",
          "unc": "//server/share"
        }'''
        data = module.parse_configuration(raw)
        self.assertEqual(data["remote"], "https://github.com/example/repo.git")
        self.assertEqual(data["path"], r"C:\Users\test\data")
        self.assertEqual(data["quote"], 'escaped " // inside string')
        self.assertEqual(data["unc"], "//server/share")
        self.config.write_text('{ // default = 123 is only a comment\n "timeout": 90 // seconds\n}')
        values, _ = module.load_configuration(self.config)
        self.assertEqual(values["interval"], 3600)
        self.assertEqual(values["timeout"], 90)
        with self.assertRaises(ValueError):
            module.parse_configuration('{"interval": 60, // trailing comma remains invalid\n}')

    def test_relative_paths_defaults_and_override_precedence(self):
        values, path = self.load({"git_repo": "../repo", "state_dir": "state", "interval": 7200},
                                 {"interval": 1800})
        self.assertEqual(values["git_repo"], str((self.root / "../repo").resolve()))
        self.assertEqual(values["archive_dir"], str(Path(values["git_repo"]) / "ai/raw/wk-me"))
        self.assertEqual(values["state_dir"], str((self.root / "state").resolve()))
        self.assertEqual(values["interval"], 1800)
        self.assertEqual(path, str(self.config.resolve()))

    def test_environment_expansion(self):
        with mock.patch.dict(module.os.environ, {"WAYLOG_TEST_ROOT": str(self.root)}):
            values, _ = self.load({"git_repo": "%WAYLOG_TEST_ROOT%/repo",
                                   "state_dir": "${WAYLOG_TEST_ROOT}/state"})
        self.assertEqual(values["git_repo"], str((self.root / "repo").resolve()))
        self.assertEqual(values["state_dir"], str((self.root / "state").resolve()))

    def test_invalid_config_rejected_before_any_process(self):
        invalid = [{"typo": 1}, {"pull_on_idle": "false"}, {"interval": True},
                   {"watch_interval": 0}, {"retry_delays": []}, {"retry_delays": [False]},
                   {"retry_max_failures": 0}, {"repo_mode": "api"}, {"git_repo": 123}]
        with mock.patch.object(module, "run", side_effect=AssertionError("must not start a process")):
            for data in invalid:
                with self.subTest(data=data), self.assertRaises(module.ServiceError):
                    self.load(data)
            with self.assertRaisesRegex(module.ServiceError, "missing"):
                module.load_configuration(self.root / "missing.json")

    def test_task_pins_paths_but_rereads_pull_and_retry_policy(self):
        import argparse
        values, path = self.load({"git_repo": "repo", "state_dir": "state"})
        service = module.Service(argparse.Namespace(**values, config_path=path, config_overrides={}))
        arguments = service.command_arguments("sync")
        self.load({"git_repo": "different-repo", "state_dir": "different-state",
                   "pull_on_idle": False, "retry_max_failures": 2, "timeout": 90})
        with mock.patch.object(module.sys, "argv", arguments[1:]), mock.patch.object(module, "Service") as mocked:
            module.main()
        args = mocked.call_args.args[0]
        self.assertEqual(args.git_repo, str(service.repo))
        self.assertEqual(args.state_dir, str(service.state))
        self.assertFalse(args.pull_on_idle)
        self.assertEqual(args.retry_max_failures, 2)
        self.assertEqual(args.timeout, 90)


class ConditionalPullTests(unittest.TestCase):
    record_git_calls = SyncTests.record_git_calls
    remote_head = SyncTests.remote_head

    def setUp(self):
        DirectSyncTests.setUp(self)
        self.service.pull_on_idle = False

    def test_idle_does_not_fetch_push_or_change_retry_state(self):
        self.service.sync()
        before = self.service.network_state()
        calls = self.record_git_calls()
        self.service.sync()
        self.assertFalse(any(args[0] in ("fetch", "push") or args[:2] == ("lfs", "push") for args in calls))
        self.assertEqual(before, self.service.network_state())
        self.assertEqual(json.loads((self.service.state / "status.json").read_text())["sync_action"], "skipped")

    def test_pending_is_retried_without_new_files(self):
        original = self.service.git
        def fail_push(*args, **kwargs):
            return (1, "simulated failure") if args[0] == "push" else original(*args, **kwargs)
        self.service.git = fail_push
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertEqual(self.service.pending(), 1)
        self.service.git = original
        self.service.sync()
        self.assertEqual(self.service.pending(), 0)

    def test_configured_retry_limit_survives_restart_and_policy_changes(self):
        self.service.retry_max_failures = 2
        self.service.retry_delays = [5400]
        original = self.service.git
        def fail_fetch(*args, **kwargs):
            if args[0] == "fetch":
                self.service.begin_network()
                raise module.ServiceError("simulated timeout")
            return original(*args, **kwargs)
        self.service.git = fail_fetch
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        self.assertEqual(self.service.network_state()["next_retry_epoch"], self.clock + 5400)
        self.clock += 5401
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        fresh = module.Service(self.service.args)
        fresh.retry_max_failures = 10
        self.clock += 86400
        with mock.patch.object(module, "run", side_effect=AssertionError("paused state must persist")):
            module.Service.sync(fresh)
        self.assertTrue(fresh.network_state()["paused"])

    def test_successful_hourly_jitter_waits_locally_without_early_network(self):
        self.service.sync()
        deadline = self.service.network_state()["next_retry_epoch"]
        self.clock = deadline - 0.5
        def wait_local(seconds):
            self.assertEqual(seconds, 0.5)
            self.clock += seconds
        with mock.patch.object(module.CANCELLED, "wait", side_effect=wait_local) as wait, \
                mock.patch.object(module, "run", side_effect=AssertionError("must not connect")):
            self.assertTrue(self.service.network_allowed())
        wait.assert_called_once()
        self.assertEqual(self.clock, deadline)


class SparseCloneTests(unittest.TestCase):
    record_git_calls = SyncTests.record_git_calls

    def setUp(self):
        SyncTests.setUp(self)
        own = self.other / "ai/raw/wk-jzx/sessions"
        other = self.other / "ai/raw/wk-me/sessions"
        own.mkdir(parents=True)
        other.mkdir(parents=True)
        (own / "session.md").write_text("own archive\n")
        (other / "foreign.md").write_text("foreign content\n")
        git(self.other, "add", ".")
        git(self.other, "commit", "-m", "two devices")
        git(self.other, "push")
        self.foreign_sha = git(self.other, "rev-parse", "HEAD:ai/raw/wk-me/sessions/foreign.md")
        git(self.remote, "config", "uploadpack.allowFilter", "true")
        args = self.service.args
        args.git_repo = str(self.root / "sparse-repo")
        args.repo_path = "ai/raw/wk-jzx"
        args.archive_dir = str(Path(args.git_repo) / args.repo_path)
        args.remote = self.remote.as_uri()
        args.repo_mode = "sparse"
        args.pull_on_idle = False
        self.service = module.Service(args)
        self.source = self.service.source
        SyncTests.install_sync_clock(self)

    def assert_foreign_not_downloaded(self):
        self.assertFalse((self.service.repo / "ai/raw/wk-me").exists())
        missing = git(self.service.repo, "rev-list", "--objects", "--all", "--missing=print")
        self.assertIn("?" + self.foreign_sha, missing.splitlines())

    def test_init_materializes_only_own_archive_then_idle_skips(self):
        self.service.init_repo()
        self.service.check_repo()
        self.assert_foreign_not_downloaded()
        self.assertEqual((self.source / "sessions/session.md").read_text(), "own archive\n")
        before = self.service.network_state()
        calls = self.record_git_calls()
        self.service.sync()
        self.assertFalse(any(args[0] in ("fetch", "push") for args in calls))
        self.assertEqual(before, self.service.network_state())

    def test_own_change_integrates_foreign_commits_without_downloading_foreign_blob(self):
        self.service.init_repo()
        (self.other / "ai/raw/wk-me/sessions/foreign.md").write_text("new foreign content\n")
        git(self.other, "add", ".")
        git(self.other, "commit", "-m", "foreign update")
        git(self.other, "push")
        self.foreign_sha = git(self.other, "rev-parse", "HEAD:ai/raw/wk-me/sessions/foreign.md")
        (self.source / "sessions/session.md").write_text("own update\n")
        self.service.sync()
        self.assertEqual(self.service.pending(), 0)
        self.assertEqual(git(self.remote, "show", "main:ai/raw/wk-jzx/sessions/session.md"), "own update")
        self.assertEqual(git(self.remote, "show", "main:ai/raw/wk-me/sessions/foreign.md"), "new foreign content")
        self.assert_foreign_not_downloaded()

    def test_existing_full_clone_is_preserved_and_refused(self):
        subprocess.check_call(["git", "clone", self.remote.as_uri(), str(self.service.repo)],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        head = git(self.service.repo, "rev-parse", "HEAD")
        before = (self.service.repo / ".git/config").read_bytes()
        with self.assertRaisesRegex(module.ServiceError, "fresh path"):
            self.service.init_repo()
        self.assertEqual(git(self.service.repo, "rev-parse", "HEAD"), head)
        self.assertEqual((self.service.repo / ".git/config").read_bytes(), before)
        self.assertFalse((self.service.state / "network-state.json").exists())

    def test_sparse_lfs_download_and_upload_stay_in_own_directory(self):
        git(self.other, "lfs", "install", "--local")
        (self.other / ".gitattributes").write_text("ai/raw/*/sessions/*.raw.jsonl filter=lfs diff=lfs merge=lfs -text\n")
        own = self.other / "ai/raw/wk-jzx/sessions/session.raw.jsonl"
        foreign = self.other / "ai/raw/wk-me/sessions/foreign.raw.jsonl"
        own.write_text('{"own": true}\n')
        foreign.write_text('{"foreign": true}\n')
        git(self.other, "add", ".")
        git(self.other, "commit", "-m", "device LFS archives")
        git(self.other, "push")
        self.service.init_repo()
        self.assertEqual((self.source / "sessions/session.raw.jsonl").read_text(), own.read_text())
        foreign_oid = module.hashlib.sha256(foreign.read_bytes()).hexdigest()
        self.assertFalse(any(path.name == foreign_oid for path in (self.service.repo / ".git/lfs/objects").rglob("*")))
        (self.source / "sessions/session.raw.jsonl").write_text('{"own": "updated"}\n')
        self.service.sync()
        pointer = git(self.remote, "show", "main:ai/raw/wk-jzx/sessions/session.raw.jsonl")
        expected_oid = module.hashlib.sha256(b'{"own": "updated"}\n').hexdigest()
        self.assertIn(expected_oid, pointer)
        self.assertTrue(any(path.name == expected_oid for path in (self.remote / "lfs/objects").rglob("*")))
        self.assert_foreign_not_downloaded()

    def test_server_without_filter_support_is_refused_and_partial_state_retained(self):
        git(self.remote, "config", "uploadpack.allowFilter", "false")
        with self.assertRaisesRegex(module.ServiceError, "ignored partial-clone"):
            self.service.init_repo()
        self.assertFalse(self.service.repo.exists())
        self.assertTrue(self.service.repo.with_name(".sparse-repo.waylog-initializing").exists())
        self.assertEqual(self.service.network_state()["consecutive_failures"], 1)
        self.clock += 3601
        with mock.patch.object(module, "run", side_effect=AssertionError("must not reclone")):
            with self.assertRaisesRegex(module.ServiceError, "Incomplete clone preserved"):
                self.service.init_repo()

    def test_invalid_lfs_scope_or_additional_sparse_paths_refused(self):
        self.service.init_repo()
        git(self.service.repo, "config", "lfs.fetchinclude", "*")
        with self.assertRaisesRegex(module.ServiceError, "LFS download scope"):
            self.service.check_repo()
        git(self.service.repo, "config", "lfs.fetchinclude", "ai/raw/wk-jzx/**")
        git(self.service.repo, "config", "lfs.fetchexclude", "*")
        with self.assertRaisesRegex(module.ServiceError, "LFS download scope"):
            self.service.check_repo()
        git(self.service.repo, "config", "lfs.fetchexclude", "")
        git(self.service.repo, "sparse-checkout", "add", "ai/raw/wk-me")
        with self.assertRaisesRegex(module.ServiceError, "exactly this machine"):
            self.service.check_repo()

    def test_local_clone_ignoring_filter_is_also_refused(self):
        self.service.args.remote = str(self.remote)
        with self.assertRaisesRegex(module.ServiceError, "ignored partial-clone"):
            self.service.init_repo()
        self.assertFalse(self.service.repo.exists())

    def test_sparse_recovery_keeps_foreign_blobs_missing(self):
        self.service.init_repo()
        raw = self.source / "sessions/large.raw.jsonl"
        raw.write_text('{"sparse": true}\n')
        original = self.service.git
        push_calls = []
        def reject_once(*args, **kwargs):
            if args[0] == "push":
                push_calls.append(args)
                if len(push_calls) == 1:
                    return 1, ("remote: error: File ai/raw/wk-jzx/sessions/large.raw.jsonl is 101.61 MB; this exceeds limit\n"
                               "remote: error: GH001: Large files detected.")
            return original(*args, **kwargs)
        self.service.git = reject_once
        with self.assertRaises(module.ServiceError):
            self.service.sync()
        status = json.loads((self.service.state / "status.json").read_text())
        self.assertEqual(status["lfs_recovery"], "ready-for-next-cycle")
        self.assertEqual(len(push_calls), 1)
        self.assert_foreign_not_downloaded()
        self.service.check_repo()
        self.service.sync()
        self.assertEqual(len(push_calls), 2)
        self.assert_foreign_not_downloaded()

    def test_custom_watch_interval_is_forwarded_and_old_binary_refused(self):
        self.service.init_repo()
        self.service.watch_interval = 45
        original_run = module.run
        def fake_help(args, **kwargs):
            if args[0] == "/unused":
                return 0, "--archive-lock-file --git-dir --interval-seconds"
            return original_run(args, **kwargs)
        with mock.patch.object(module, "run", side_effect=fake_help):
            command = self.service.watch_command()
        self.assertEqual(command[command.index("--interval-seconds") + 1], "45")
        with mock.patch.object(module, "run", return_value=(0, "--archive-lock-file --git-dir")):
            with self.assertRaisesRegex(module.ServiceError, "lacks native watch"):
                self.service.watch_command()


class PortableIndexTests(unittest.TestCase):
    def setUp(self):
        DirectSyncTests.setUp(self)
        (self.source / "indexes").mkdir()
        self.index = self.source / "indexes/sessions.jsonl"

    def test_producer_absolute_paths_resolve_after_clone_relocation(self):
        for reference in ("D:\\old\\repo\\ai\\raw\\wk-me\\sessions\\session.md",
                          "/old/repo/ai/raw/wk-me/sessions/session.md", "sessions/session.md"):
            with self.subTest(reference=reference):
                self.index.write_text(json.dumps({"markdown_path": reference}))
                self.service.validate_snapshot(self.source)

    def test_invalid_or_escaping_index_refs_refused(self):
        for reference in (123, "", "/old/sessions/../../manifest.json", "../manifest.json",
                          "/old/sessions//tmp/file.md", "/old/file.md", "sessions/missing.md"):
            with self.subTest(reference=reference), self.assertRaises(module.ServiceError):
                self.index.write_text(json.dumps({"markdown_path": reference}))
                self.service.validate_snapshot(self.source)

    def test_current_index_manifest_is_validated(self):
        (self.source / "indexes/manifest.json").write_text("{broken")
        with self.assertRaises(ValueError):
            self.service.validate_snapshot(self.source)


class CliRoutingTests(unittest.TestCase):
    def parse_macos_status(self, *options):
        from types import SimpleNamespace
        with mock.patch.object(module, "os", SimpleNamespace(name="posix", environ={})), \
                mock.patch.object(module.sys, "platform", "darwin"), \
                mock.patch.object(module.sys, "argv", ["waylog-service.py", *options, "status"]), \
                mock.patch.object(module, "Service") as service:
            module.main()
        service.return_value.status.assert_called_once_with()
        service.return_value.sync.assert_not_called()
        service.return_value.launch.assert_not_called()
        return service.call_args.args[0]

    def test_macos_defaults_to_archive_existing_repository(self):
        args = self.parse_macos_status()
        self.assertEqual(args.git_repo, module.DEFAULT_REPO)
        self.assertEqual(args.archive_dir, module.DEFAULT_SOURCE)
        self.assertFalse(args.isolated)

    def test_macos_isolated_option_keeps_legacy_mode(self):
        args = self.parse_macos_status("--isolated")
        self.assertIsNone(args.git_repo)
        self.assertTrue(args.isolated)

    def test_custom_existing_repository_option_is_preserved(self):
        args = self.parse_macos_status("--git-repo", "/tmp/custom-archive-repo")
        self.assertEqual(args.git_repo, str(Path("/tmp/custom-archive-repo").resolve()))
        self.assertFalse(args.isolated)


class MacCalendarCooldownTests(unittest.TestCase):
    def setUp(self):
        import argparse
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        args = argparse.Namespace(**module.config_defaults(windows=False))
        args.git_repo = None
        args.archive_dir = str(Path(self.temp.name) / 'archive')
        args.state_dir = str(Path(self.temp.name) / 'state')
        self.service = module.Service(args)
        self.hour = module.datetime.datetime.now().astimezone().replace(
            minute=0, second=0, microsecond=0).timestamp()

    def finish(self, success, attempt, end, failures=1):
        self.service.save_network_state(dict(consecutive_failures=failures,
            paused=failures >= self.service.retry_max_failures,
            in_flight=True, last_attempt_epoch=attempt,
            next_retry_epoch=attempt + self.service.retry_wait(failures)))
        self.service._network_active = True
        with mock.patch.object(module.sys, 'platform', 'darwin'), \
                mock.patch.object(module.time, 'time', return_value=end):
            self.service.finish_network(success)
        return self.service.network_state()

    def test_mac_success_allows_next_hour_but_not_same_hour(self):
        data = self.finish(True, self.hour + 12, self.hour + 1500)
        self.assertEqual(data['next_retry_epoch'], self.hour + 3600)
        with mock.patch.object(module.time, 'time', return_value=self.hour + 1800):
            self.assertFalse(self.service.network_allowed())
        with mock.patch.object(module.time, 'time', return_value=self.hour + 3600):
            self.assertTrue(self.service.network_allowed())

    def test_mac_failure_keeps_one_and_two_hour_delays_and_pause(self):
        for failures, delay in ((1, 3600), (2, 7200), (3, 7200)):
            data = self.finish(False, self.hour + 12, self.hour + 1500, failures)
            self.assertEqual(data['next_retry_epoch'], self.hour + 1500 + delay)
            with mock.patch.object(module.time, 'time', return_value=self.hour + 3600):
                self.assertFalse(self.service.network_allowed())
        with mock.patch.object(module.time, 'time', return_value=data['next_retry_epoch'] + 1):
            self.assertFalse(self.service.network_allowed())

    def test_windows_success_and_custom_spacing_are_unchanged(self):
        with mock.patch.object(module.sys, 'platform', 'win32'):
            self.assertEqual(self.service.success_deadline(self.hour + 12), self.hour + 3612)
        self.service.network_spacing = 7200
        with mock.patch.object(module.sys, 'platform', 'darwin'):
            self.assertEqual(self.service.success_deadline(self.hour + 12), self.hour + 7212)


if __name__ == "__main__":
    unittest.main()
