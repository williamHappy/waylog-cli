#!/usr/bin/env python3
"""Waylog watch and hourly Git sync for macOS and Windows."""
import argparse
from collections import Counter
import contextlib
import datetime
import os
if os.name == "nt":
    import msvcrt
else:
    import fcntl
import hashlib
import json
import logging
import re
from logging.handlers import RotatingFileHandler
from pathlib import Path
import plistlib
import random
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time

DEFAULT_SOURCE = "/Users/wangct/workspace/wk-me/i-raw-data/ai/raw/wk-me"
DEFAULT_REPO = "/Users/wangct/workspace/wk-me/i-raw-data"
DEFAULT_REMOTE = "git@github.com:williamHappy/i-raw-data.git"
ARCHIVE_ITEMS = ("sessions", "indexes", "browser-history", "manifest.json")
CANCELLED = threading.Event()


def process_options():
    if os.name == "nt":
        return {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.CREATE_NO_WINDOW}
    return {"start_new_session": True}


def terminate_tree(process, force=False):
    if process.poll() is not None:
        return
    if os.name == "nt":
        subprocess.run(["taskkill.exe", "/PID", str(process.pid), "/T", "/F"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       creationflags=subprocess.CREATE_NO_WINDOW, timeout=15, check=False)
    else:
        os.killpg(process.pid, signal.SIGKILL if force else signal.SIGTERM)


def windows_job():
    """Attach this worker to a kill-on-close Job; Task Scheduler stops its children too."""
    import ctypes
    from ctypes import wintypes
    class Basic(ctypes.Structure):
        _fields_ = [("ProcessTime", ctypes.c_longlong), ("JobTime", ctypes.c_longlong),
                    ("Flags", wintypes.DWORD), ("MinWorkingSet", ctypes.c_size_t),
                    ("MaxWorkingSet", ctypes.c_size_t), ("ActiveProcesses", wintypes.DWORD),
                    ("Affinity", ctypes.c_size_t), ("Priority", wintypes.DWORD),
                    ("Scheduling", wintypes.DWORD)]
    class IO(ctypes.Structure):
        _fields_ = [(name, ctypes.c_ulonglong) for name in
                    ("ReadOps", "WriteOps", "OtherOps", "ReadBytes", "WriteBytes", "OtherBytes")]
    class Extended(ctypes.Structure):
        _fields_ = [("Basic", Basic), ("IO", IO), ("ProcessMemory", ctypes.c_size_t),
                    ("JobMemory", ctypes.c_size_t), ("PeakProcessMemory", ctypes.c_size_t),
                    ("PeakJobMemory", ctypes.c_size_t)]
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    kernel.CreateJobObjectW.restype = wintypes.HANDLE
    kernel.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    kernel.SetInformationJobObject.restype = wintypes.BOOL
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    kernel.AssignProcessToJobObject.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    job = kernel.CreateJobObjectW(None, None)
    if not job:
        raise ctypes.WinError(ctypes.get_last_error())
    limits = Extended()
    limits.Basic.Flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    if not kernel.SetInformationJobObject(job, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
        code = ctypes.get_last_error()
        kernel.CloseHandle(job)
        raise ctypes.WinError(code)
    if not kernel.AssignProcessToJobObject(job, kernel.GetCurrentProcess()):
        code = ctypes.get_last_error()
        kernel.CloseHandle(job)
        raise ctypes.WinError(code)
    # Do not close this handle while this worker is alive: that would kill the worker itself.
    return job


class ServiceError(Exception):
    pass


def config_defaults(windows=None):
    windows = os.name == "nt" if windows is None else windows
    return dict(
        git_repo="D:/wangct/wk-me/i-raw-data-sparse" if windows else DEFAULT_REPO,
        archive_dir=None, repo_path="ai/raw/wk-jzx" if windows else "ai/raw/wk-me",
        repo_mode="sparse" if windows else "full", pull_on_idle=not windows,
        remote=DEFAULT_REMOTE, branch="main",
        state_dir=str(Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "waylog-service")
            if windows else str(Path.home() / "Library/Application Support/waylog-service"),
        python="python" if windows else "python3", git="git",
        ssh=str(Path(os.environ.get("WINDIR", "C:/Windows")) / "System32/OpenSSH/ssh.exe")
            if windows else "/usr/bin/ssh",
        waylog=str(Path.home() / ".cargo/bin" / ("waylog.exe" if windows else "waylog")),
        interval=3600, timeout=300, watch_interval=30, no_browser=False,
        retry_max_failures=3, retry_delays=[3600, 7200], minimum_network_interval=3600,
        git_name="williamHappy", git_email="williamHappy@users.noreply.github.com")


def expand_config_path(value, base):
    value = re.sub(r"%([^%]+)%", lambda match: os.environ.get(match[1], match[0]), value)
    value = re.sub(r"\$(?:\{([^}]+)\}|([A-Za-z_][A-Za-z0-9_]*))",
                   lambda match: os.environ.get(match[1] or match[2], match[0]), value)
    path = Path(value).expanduser()
    return str((base / path).resolve() if not path.is_absolute() else path.resolve())


def parse_configuration(text):
    """Accept JSON with // comments without changing quoted URLs or Windows paths."""
    tokens = r'"(?:\\.|[^"\\])*"|//[^\r\n]*'
    def uncomment(match):
        value = match.group(0)
        return value if value.startswith('"') else " " * len(value)
    return json.loads(re.sub(tokens, uncomment, text))


def load_configuration(path=None, overrides=None):
    defaults = config_defaults()
    supplied = dict(overrides or {})
    default_file = Path(__file__).with_name(
        "waylog-service.windows.json" if os.name == "nt" else "waylog-service.macos.json")
    config_file = Path(path).expanduser().resolve() if path else default_file.resolve()
    data = {}
    if config_file.exists():
        try:
            data = parse_configuration(config_file.read_text(encoding="utf-8-sig"))
        except (OSError, ValueError) as error:
            raise ServiceError(f"Cannot read configuration: {config_file}") from error
        if not isinstance(data, dict):
            raise ServiceError("Configuration must be a JSON object")
        unknown = set(data) - set(defaults)
        if unknown:
            raise ServiceError(f"Unknown configuration fields: {', '.join(sorted(unknown))}")
    elif path:
        raise ServiceError(f"Configuration file missing: {config_file}")
    values = dict(defaults, **data)
    values.update(supplied)
    for name in ("pull_on_idle", "no_browser"):
        if type(values[name]) is not bool:
            raise ServiceError(f"{name} must be a JSON boolean")
    for name, minimum in (("interval", 60), ("timeout", 1), ("watch_interval", 1),
                          ("retry_max_failures", 1), ("minimum_network_interval", 60)):
        if type(values[name]) is not int or values[name] < minimum:
            raise ServiceError(f"{name} must be an integer >= {minimum}")
    if (not isinstance(values["retry_delays"], list) or not values["retry_delays"]
            or any(type(delay) is not int or delay < 1 for delay in values["retry_delays"])):
        raise ServiceError("retry_delays must be a nonempty array of positive integer seconds")
    if values["repo_mode"] not in ("full", "sparse"):
        raise ServiceError("repo_mode must be full or sparse")
    for name in ("git_repo", "archive_dir"):
        if values[name] is not None and (not isinstance(values[name], str) or not values[name]):
            raise ServiceError(f"{name} must be a nonempty path or null")
    for name in set(defaults) - {"git_repo", "archive_dir", "pull_on_idle", "no_browser", "interval",
                               "timeout", "watch_interval", "retry_max_failures", "retry_delays",
                               "minimum_network_interval"}:
        if not isinstance(values[name], str) or not values[name]:
            raise ServiceError(f"{name} must be a nonempty string")
    base = config_file.parent
    for name in ("git_repo", "archive_dir", "state_dir", "waylog"):
        if values[name] is not None:
            values[name] = expand_config_path(values[name], base)
    for name in ("python", "git", "ssh"):
        value = values[name]
        if any(mark in value for mark in ("/", "\\", "%", "~", "$")):
            values[name] = expand_config_path(value, base)
    if values["archive_dir"] is None:
        values["archive_dir"] = str(Path(values["git_repo"] or defaults["git_repo"]) / values["repo_path"])
    return values, str(config_file) if config_file.exists() else None


def run(args, cwd=None, timeout=180, check=True, env=None, machine_output=False):
    """Bound the entire process group, including SSH and LFS children."""
    process = subprocess.Popen(args, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE if machine_output else subprocess.STDOUT,
                               text=True, encoding="utf-8", errors="replace", **process_options())
    previous_handler = signal.getsignal(signal.SIGTERM)
    def interrupted(signum, frame):
        raise ServiceError(f"Interrupted: {args[0]}")
    signal.signal(signal.SIGTERM, interrupted)
    try:
        deadline = time.monotonic() + timeout
        while True:
            if CANCELLED.is_set():
                raise ServiceError(f"Interrupted: {args[0]}")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(args, timeout)
            try:
                output, diagnostic = process.communicate(timeout=min(0.5, remaining))
                break
            except subprocess.TimeoutExpired:
                continue
    except BaseException as error:
        if process.poll() is None:
            terminate_tree(process)
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            terminate_tree(process, force=True)
            process.communicate()
        if isinstance(error, subprocess.TimeoutExpired):
            raise ServiceError(f"Timed out: {args[0]}") from error
        raise
    finally:
        signal.signal(signal.SIGTERM, previous_handler)
    if check and process.returncode:
        details = output + (diagnostic or "")
        raise ServiceError(f"Command failed ({process.returncode}): {shlex.join(args)}\n{details.strip()}")
    if machine_output and diagnostic:
        print(diagnostic.rstrip(), file=sys.stderr)
    return process.returncode, output if machine_output else output.strip()


def stamp():
    return datetime.datetime.now().astimezone().isoformat(timespec="seconds")


def is_link(path):
    if path.is_symlink():
        return True
    if os.name == "nt" and path.exists():
        # Junctions are reparse points too; Python 3.9's is_symlink does not detect them.
        return bool(getattr(path.lstat(), "st_file_attributes", 0) & 0x400)
    return False


class Service:
    def __init__(self, args):
        self.args = args
        self.source = Path(args.archive_dir).expanduser().resolve()
        self.state = Path(args.state_dir).expanduser().resolve()
        existing_repo = getattr(args, "git_repo", None)
        self.direct = bool(existing_repo)
        self.repo_mode = getattr(args, "repo_mode", "full")
        self.pull_on_idle = getattr(args, "pull_on_idle", True)
        self.watch_interval = getattr(args, "watch_interval", 30)
        self.retry_max_failures = getattr(args, "retry_max_failures", 3)
        self.retry_delays = getattr(args, "retry_delays", [3600, 7200])
        self.network_spacing = max(getattr(args, "minimum_network_interval", 3600), args.interval)
        if self.repo_mode == "sparse" and not self.direct:
            raise ServiceError("Sparse mode requires git_repo; isolated mode is full only")
        self.repo = Path(existing_repo).expanduser().resolve() if self.direct else self.state / "repo"
        # Git always reports repository paths with /, including on Windows.
        self.remote_path = Path(args.repo_path.replace("\\", "/"))
        self.git_path = getattr(args, "git", None) or shutil.which("git") or "git"
        self.ssh_path = getattr(args, "ssh", None) or (
            str(Path(os.environ.get("WINDIR", "C:/Windows")) / "System32/OpenSSH/ssh.exe")
            if os.name == "nt" else "/usr/bin/ssh")
        if (self.remote_path.anchor or not self.remote_path.parts
                or any(p.lower() in ("..", ".git") for p in self.remote_path.parts)):
            raise ServiceError("repo-path must be a nonempty relative path without .. or .git")
        if self.state == self.source or self.source in self.state.parents or self.state in self.source.parents:
            raise ServiceError("State and archive directories must be separate, non-nested directories")
        if self.direct:
            if self.source != (self.repo / self.remote_path).resolve():
                raise ServiceError("archive-dir must equal git-repo/repo-path in direct mode")
            if self.state == self.repo or self.repo in self.state.parents:
                raise ServiceError("State directory must be outside the existing Git repository")
        if args.interval < 60:
            raise ServiceError("interval must be at least 60 seconds")
        if args.remote.startswith("https://") and "@" in args.remote:
            raise ServiceError("Do not embed credentials in the remote URL")
        self.env = dict(os.environ, GIT_TERMINAL_PROMPT="0", GIT_LFS_SKIP_SMUDGE="0" if self.direct else "1")
        # Apple SSH can retrieve an encrypted key's passphrase from Keychain.
        self.env["GIT_SSH_COMMAND"] = (shlex.quote(self.ssh_path.replace("\\", "/"))
            + " -o BatchMode=yes -o ConnectTimeout=15 -o StrictHostKeyChecking=yes")
        self.env["GIT_EDITOR"] = "true"
        self.env["GIT_SEQUENCE_EDITOR"] = "true"
        self.env["LC_ALL"] = "C"
        self._network_active = False
        self._lfs_ready_for_push = False

    def git(self, *args, check=True):
        timeout = self.args.timeout
        if getattr(self, '_push_deadline', None) is not None:
            remaining = self._push_deadline - time.monotonic()
            if remaining <= 0:
                raise ServiceError("Push reconciliation time budget exhausted; local commits retained")
            timeout = min(timeout, remaining)
        options = ["-c", "rebase.autoStash=false", "-c", "merge.autoStash=false",
                   "-c", "lfs.transfer.maxretries=1",
                   "-c", "lfs.ssh.retries=1", "-c", "lfs.transfer.maxverifies=1"]
        if not self.direct:
            options += ["-c", f"core.hooksPath={(self.repo / '.git/hooks').as_posix()}", "-c", "commit.gpgsign=false"]
        elif args and args[0] in ("fetch", "push", "lfs", "rebase"):
            # remote.*.url is multi-valued: -c adds a URL, and fetch still uses
            # the first saved URL. Rewrite saved URLs for this process instead.
            _, saved_urls = run([self.git_path, "-C", str(self.repo), "config", "--get-all",
                                 "remote.origin.url"], timeout=timeout,
                                check=False, env=self.env)
            for saved_url in saved_urls.splitlines():
                if saved_url and saved_url != self.args.remote:
                    options += ["-c", f"url.{self.args.remote}.insteadOf={saved_url}"]
            options += ["-c", f"remote.origin.url={self.args.remote}", "-c", f"remote.origin.pushurl={self.args.remote}"]
        if args and args[0] in ("fetch", "push", "lfs") and not (args[0] == "lfs" and args[1:] == ("version",)):
            self.begin_network()
        env = self.env
        if args and args[0] == "push" and self.direct and self._lfs_ready_for_push:
            env = dict(self.env, GIT_LFS_SKIP_PUSH="1")
        if getattr(self, '_push_deadline', None) is not None:
            timeout = min(timeout, self._push_deadline - time.monotonic())
            if timeout <= 0:
                raise ServiceError("Push reconciliation time budget exhausted; local commits retained")
        return run([self.git_path, "-C", str(self.repo), *options, *args], timeout=timeout,
                   check=check, env=env, machine_output="-z" in args)

    def network_state(self):
        path = self.state / "network-state.json"
        if not path.exists():
            return {"consecutive_failures": 0, "paused": False, "next_retry_epoch": 0.0,
                    "last_attempt_epoch": 0.0, "in_flight": False}
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
            if (type(data["consecutive_failures"]) is not int or data["consecutive_failures"] < 0
                    or type(data["paused"]) is not bool or type(data["in_flight"]) is not bool
                    or not isinstance(data["next_retry_epoch"], (int, float))
                    or not isinstance(data["last_attempt_epoch"], (int, float))
                    or not 0 <= data["next_retry_epoch"] < float("inf")
                    or not 0 <= data["last_attempt_epoch"] < float("inf")):
                raise ValueError("invalid retry fields")
            return data
        except (KeyError, TypeError, ValueError) as error:
            raise ServiceError("Invalid network-state.json; refusing network access until inspected") from error

    def save_network_state(self, data):
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        data["next_retry_at"] = datetime.datetime.fromtimestamp(data["next_retry_epoch"], datetime.timezone.utc).astimezone().isoformat()
        temporary = self.state / "network-state.tmp"
        temporary.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        temporary.replace(self.state / "network-state.json")

    def network_allowed(self):
        data = self.network_state()
        if data["paused"] or data["consecutive_failures"] >= self.retry_max_failures:
            self.record(result="network-paused")
            reason = data.get("pause_reason") or f"{data['consecutive_failures']} failed/interrupted attempts"
            print(f"Network sync paused: {reason}. Inspect logs, then use resume-sync.")
            return False
        remaining = data["next_retry_epoch"] - time.time()
        # Hourly schedulers have subsecond jitter. Wait out a tiny successful-round
        # cooldown rather than skipping another entire hour; never relax the spacing.
        if data["consecutive_failures"] == 0 and 0 < remaining <= 2:
            CANCELLED.wait(remaining)
        if time.time() < data["next_retry_epoch"]:
            self.record(result="cooldown")
            print(f"Network cooldown until {data.get('next_retry_at', data['next_retry_epoch'])}; no network access.")
            return False
        return True

    def begin_network(self):
        if self._network_active:
            return
        if not self.network_allowed():
            raise ServiceError("Network access deferred by retry policy")
        data = self.network_state()
        # Reserve BEFORE spawning a child. A kill/reboot counts as an interrupted attempt.
        data["consecutive_failures"] += 1
        data["paused"] = data["consecutive_failures"] >= self.retry_max_failures
        data["last_attempt_epoch"] = time.time()
        data["next_retry_epoch"] = time.time() + self.retry_wait(data["consecutive_failures"])
        data["in_flight"] = True
        self.save_network_state(data)
        self._network_active = True

    def finish_network(self, success):
        if not self._network_active:
            return
        data = self.network_state()
        data["in_flight"] = False
        if success:
            data["consecutive_failures"] = 0
            data["paused"] = False
            data.pop("pause_reason", None)
            data["next_retry_epoch"] = self.success_deadline(data["last_attempt_epoch"])
        else:
            data["next_retry_epoch"] = max(data["next_retry_epoch"],
                time.time() + self.retry_wait(data["consecutive_failures"]))
        self.save_network_state(data)
        self._network_active = False
        self._lfs_ready_for_push = False

    def success_deadline(self, attempt_epoch):
        # Mac's default scheduler runs at Minute=0. Successful rounds occupy a
        # calendar hour, so preparation/launch latency cannot skip the next hour.
        # Failure/interruption cooldowns keep their strict elapsed-time policy.
        if sys.platform == "darwin" and self.args.interval == 3600 and self.network_spacing == 3600:
            hour = datetime.datetime.fromtimestamp(attempt_epoch).astimezone().replace(
                minute=0, second=0, microsecond=0)
            return hour.timestamp() + 3600
        return attempt_epoch + self.network_spacing

    def retry_wait(self, failures):
        return max(self.network_spacing, self.retry_delays[min(max(0, failures - 1), len(self.retry_delays) - 1)])

    def check_push_rejection(self, output):
        # Repair only the script-owned unpublished history; never immediately retransmit.
        if self.push_failure_kind(output) == "large-file":
            data = self.network_state()
            data["paused"] = True
            data["pause_reason"] = "GitHub GH001 rejected large files; migrate/fix unpublished history before resuming"
            self.save_network_state(data)
            if self.direct:
                self.recover_large_files(output)

    @staticmethod
    def push_failure_kind(output):
        # GitHub includes GH001 in warnings too. Only error diagnostics prove
        # rejection by the size hook; accompanying warnings never take priority.
        if re.search(r"(?im)^remote:\s*error:\s*GH001:.*large files detected", output):
            return "large-file"
        for line in output.splitlines():
            if (line.startswith("!\t") or "[rejected]" in line or "[remote rejected]" in line):
                if ("(fetch first)" in line or "(non-fast-forward)" in line
                        or re.search(r"cannot lock ref 'refs/heads/[^']+': is at [0-9a-f]+ but expected [0-9a-f]+", line)):
                    return "remote-advanced"
        return "other"

    def push_with_retry(self, reconcile=None, revision=None):
        """One reserved network round, at most three ordinary push attempts.

        Reconciliation may change commit identity, but never force pushes. The
        frozen-report transport supplies its own durable merge-only callback.
        """
        self._push_deadline = time.monotonic() + self.args.timeout
        try:
            for attempt in range(1, 4):
                commit = revision() if revision else self.git("rev-parse", "HEAD")[1]
                if self.direct:
                    self.git("lfs", "push", "origin", commit)
                    self._lfs_ready_for_push = True
                code, output = self.git("push", "--porcelain", "origin",
                                        f"{commit}:refs/heads/{self.args.branch}", check=False)
                warnings = [line for line in output.splitlines() if line.startswith("remote: warning:")]
                kind = self.push_failure_kind(output) if code else None
                self.record(push_attempts=attempt, push_failure_kind=kind, push_warnings=warnings)
                if not code:
                    return commit
                if kind != "remote-advanced":
                    self.check_push_rejection(output)
                    raise ServiceError(f"Push failed; local commits retained.\n{output}")
                if attempt == 3:
                    raise ServiceError(f"Remote advanced during all 3 push attempts; local commits retained.\n{output}")
                delay = random.uniform(0.2, 1.0) * attempt
                if time.monotonic() + delay >= self._push_deadline:
                    raise ServiceError("Push reconciliation time budget exhausted; local commits retained")
                if CANCELLED.wait(delay):
                    raise ServiceError("Interrupted before push reconciliation; local commits retained")
                if reconcile:
                    reconcile()
                else:
                    self.integrate()
                    self.check_repo()
                    if self.direct:
                        self.verify_direct_pending()
                self.record(last_pull=stamp())
        finally:
            self._push_deadline = None

    def recover_large_files(self, output):
        prefix = self.remote_path.as_posix() + "/"
        reported = sorted(set(re.findall(
            r"(?m)^remote:\s+error:\s+File (.+?) is [0-9.]+ (?:MB|MiB); this exceeds", output)))
        # Exact, simple paths prevent a server message from broadening a migration glob.
        if not reported or any(not name.startswith(prefix) or
                ".." in Path(name).parts or any(char in name for char in ',*?[]\\\r\n"')
                for name in reported):
            self.record(lfs_recovery="needs-manual-review")
            return
        self.check_repo()
        self.verify_direct_pending()
        old_head = self.git("rev-parse", "HEAD")[1]
        before = self.inventory(self.source)
        refs = dict(line.split(" ", 1) for line in self.git(
            "for-each-ref", "--format=%(refname) %(objectname)")[1].splitlines())
        pending = set(self.git("rev-list", f"origin/{self.args.branch}..HEAD")[1].splitlines())
        for ref in refs:
            if ref.startswith("refs/tags/") and self.git("rev-parse", ref + "^{}")[1] in pending:
                self.record(lfs_recovery="needs-tag-review")
                return  # A tag may already have been published independently; preserve it.
        backups = self.state / "lfs-backups"
        backups.mkdir(parents=True, exist_ok=True, mode=0o700)
        token = datetime.datetime.now().strftime("%Y%m%dT%H%M%S%f")
        bundle = backups / (token + ".bundle")
        object_map = backups / (token + ".csv")
        metadata = backups / (token + ".json")
        metadata.write_text(json.dumps(dict(old_head=old_head, refs=refs, files=reported,
                                            git_repo=str(self.repo)), indent=2) + "\n", encoding="utf-8")
        try:
            # Bundles are independent of refs, which git-lfs may also rewrite during import.
            partial = bundle.with_suffix(".partial")
            self.git("bundle", "create", str(partial), f"refs/heads/{self.args.branch}",
                     f"^refs/remotes/origin/{self.args.branch}")
            self.git("bundle", "verify", str(partial))
            partial.replace(bundle)
            if self.git("rev-parse", "HEAD")[1] != old_head:
                raise ServiceError("Repository changed during LFS backup")
            self.record(lfs_recovery="migrating", lfs_backup=str(bundle))
            # Enable local filters while preserving the user's existing hooks.
            self.git("lfs", "install", "--local", "--skip-repo")
            exclusions = ["--exclude-ref=" + ref for ref in refs
                          if ref.startswith(("refs/remotes/", "refs/tags/"))]
            self.git("lfs", "migrate", "import", "--yes", "--skip-fetch",
                     "--include=" + ",".join(reported), f"--include-ref=refs/heads/{self.args.branch}",
                     "--object-map=" + str(object_map), *exclusions)
            mapping = dict(line.split(",") for line in object_map.read_text().splitlines() if line)
            new_head = self.git("rev-parse", "HEAD")[1]
            if new_head == old_head or mapping.get(old_head) != new_head:
                raise ServiceError("LFS migration did not replace the rejected history")
            # git-lfs updates refs pointing at mapped commits; restore all except this branch.
            for ref, old in refs.items():
                if ref == f"refs/heads/{self.args.branch}":
                    continue
                current = self.git("rev-parse", "--verify", ref)[1]
                if current != old:
                    if current != mapping.get(old):
                        raise ServiceError("Another Git operation changed a reference during LFS recovery")
                    self.git("update-ref", ref, old, current)
            # migrate import leaves pointers in the working tree. Restore only these
            # files from the local LFS cache; checkout never downloads missing objects.
            self.git("lfs", "checkout", "--", *reported)
            self.check_repo()
            self.verify_direct_pending()
            if before != self.inventory(self.source):
                raise ServiceError("Archive contents changed during LFS recovery")
            data = self.network_state()
            data["paused"] = data["consecutive_failures"] >= self.retry_max_failures
            if data["paused"]:
                data["pause_reason"] = "LFS history repaired, but the configured retry limit was reached"
            else:
                data.pop("pause_reason", None)
            self.save_network_state(data)
            self.record(lfs_recovery="ready-for-next-cycle", lfs_backup=str(bundle),
                        lfs_recovery_files=reported, lfs_old_head=old_head, lfs_new_head=new_head,
                        pending_commits=self.pending())
            print("Large files migrated locally to LFS. Backup retained; upload deferred to the next allowed cycle.")
        except Exception as error:
            (self.state / "BLOCKED").write_text(
                f"LFS recovery failed: {error}\nOriginal HEAD: {old_head}\nBackup: {bundle}\nMetadata: {metadata}\n",
                encoding="utf-8")
            self.record(lfs_recovery="failed", lfs_backup=str(bundle))
            raise ServiceError(f"LFS recovery failed; sync paused. Inspect BLOCKED and backup {bundle}: {error}") from error

    def resume_sync(self):
        with self.lock("sync"):
            data = self.network_state()
            data["consecutive_failures"] = 0
            data["paused"] = False
            data.pop("pause_reason", None)
            data["in_flight"] = False
            # Explicit resume does not permit immediate repeated network attempts.
            data["next_retry_epoch"] = max(time.time(), data["last_attempt_epoch"] + self.network_spacing)
            self.save_network_state(data)
            self.record(result="retry-resumed")
            print("Network retries resumed; minimum cooldown preserved. No sync or push was performed.")

    def record(self, **updates):
        path = self.state / "status.json"
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (FileNotFoundError, ValueError):
            data = {}
        data.update(updates)
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        temporary.replace(path)

    @contextlib.contextmanager
    def lock(self, name):
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        with (self.state / f"{name}.lock").open("a+b") as handle:
            try:
                if os.name == "nt":
                    if handle.tell() == 0:
                        handle.write(b"\0")
                        handle.flush()
                    handle.seek(0)
                    msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                raise ServiceError(f"{name} is already running")
            try:
                yield
            finally:
                if os.name == "nt":
                    handle.seek(0)
                    msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)

    def inventory(self, root):
        result = {}
        for item in ARCHIVE_ITEMS:
            base = root / item
            paths = [base]
            while paths:
                path = paths.pop()
                if is_link(path):
                    raise ServiceError(f"Symlink not allowed: {path}")
                if path.is_dir():
                    paths.extend(sorted(path.iterdir(), reverse=True))
                elif path.is_file():
                    result[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
        return result

    @contextlib.contextmanager
    def archive_access(self):
        """Wait for a whole collection cycle; both direct-mode watch and sync use this lock."""
        deadline = time.monotonic() + self.args.timeout
        while True:
            manager = self.lock("archive-access")
            try:
                manager.__enter__()
                break
            except ServiceError:
                if CANCELLED.is_set():
                    raise ServiceError("Interrupted while waiting for archive access")
                if time.monotonic() >= deadline:
                    raise ServiceError("Archive is busy; no Git operation performed")
                CANCELLED.wait(0.25)
        try:
            if CANCELLED.is_set():
                raise ServiceError("Interrupted before archive access")
            yield
        finally:
            manager.__exit__(None, None, None)

    def snapshot(self, target):
        for attempt in range(3):
            try:
                before = self.inventory(self.source)
                if not before:
                    raise ServiceError("Archive is empty; refusing to synchronize")
                for child in target.iterdir():
                    shutil.rmtree(child) if child.is_dir() else child.unlink()
                for relative in before:
                    destination = target / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(self.source / relative, destination)
                if before != self.inventory(self.source) or before != self.inventory(target):
                    raise ServiceError("Archive changed during snapshot")
                self.validate_snapshot(target)
                return
            except (OSError, ValueError, ServiceError) as error:
                if attempt == 2:
                    raise ServiceError(f"Cannot obtain a stable archive snapshot: {error}")
                time.sleep(1)

    def validate_snapshot(self, root):
        for manifest in (root / "manifest.json", root / "indexes/manifest.json"):
            if manifest.exists():
                json.loads(manifest.read_text(encoding="utf-8"))
        indexes = root / "indexes"
        for path in indexes.glob("*.jsonl"):
            for line in path.read_text(encoding="utf-8").splitlines():
                if not line.strip():
                    continue
                entry = json.loads(line)
                if not isinstance(entry, dict):
                    raise ServiceError("Index entry must be a JSON object")
                for field in ("markdown_path", "raw_path"):
                    if field not in entry:
                        continue
                    if not isinstance(entry[field], str) or not entry[field]:
                        raise ServiceError("Index reference must be a nonempty string")
                    raw_reference = entry[field].replace("\\", "/")
                    referenced = Path(raw_reference)
                    if referenced.is_absolute() or re.match(r"^[A-Za-z]:/", raw_reference):
                        try:
                            relative = referenced.relative_to(self.source)
                        except ValueError:
                            # A cloned archive retains the producing machine's absolute paths.
                            # Resolve only a supported archive suffix, never an arbitrary basename.
                            relative = None
                            for folder in ("sessions", "browser-history"):
                                marker = "/" + folder + "/"
                                if marker in raw_reference:
                                    relative = Path(folder) / raw_reference.rsplit(marker, 1)[1]
                                    break
                            if relative is None:
                                raise ServiceError("Index absolute path has no supported archive suffix")
                    else:
                        relative = referenced
                    if relative.anchor or ".." in relative.parts or not (root / relative).is_file():
                        raise ServiceError(f"Index references a missing file: {relative}")

    def check_repo(self, allow_archive=False):
        if self.direct:
            top = Path(self.git("rev-parse", "--show-toplevel")[1]).resolve()
            if top != self.repo:
                raise ServiceError("git-repo must be the existing repository root")
        stored_remote = self.git("remote", "get-url", "origin")[1]
        def github_repo(url):
            match = re.fullmatch(r"(?:git@github\.com:|https://github\.com/|ssh://git@github\.com/)([^/]+/[^/]+?)(?:\.git)?/?", url)
            return match.group(1).lower() if match else None
        if stored_remote != self.args.remote and not (
                self.direct and github_repo(stored_remote) and github_repo(stored_remote) == github_repo(self.args.remote)):
            raise ServiceError("Repository origin differs from configured repository")
        if self.git("branch", "--show-current")[1] != self.args.branch:
            raise ServiceError("Repository is on a different branch")
        if self.direct:
            self.check_repo_layout()
        if self.direct and allow_archive:
            if self.git("diff", "--cached", "--quiet", check=False)[0] != 0:
                raise ServiceError("Index already contains staged changes; refusing to include them")
            names = self.git("diff", "--name-only", "--no-renames", "-z", "HEAD")[1].split("\0")
            names += self.git("ls-files", "--others", "--exclude-standard", "-z")[1].split("\0")
            prefix = self.remote_path.as_posix() + "/"
            if any(name and not name.startswith(prefix) for name in names):
                raise ServiceError("Other directories have uncommitted/untracked changes; leaving them untouched")
            if any(name and name[len(prefix):].split("/")[0] not in ARCHIVE_ITEMS for name in names):
                raise ServiceError("Archive has changes outside the supported archive files; resolve manually")
            deleted = self.git("diff", "--name-only", "--diff-filter=D", "-z", "HEAD", "--", self.remote_path.as_posix())[1]
            if deleted:
                raise ServiceError("Archive contains tracked deletions; automatic deletion is disabled, resolve manually")
        elif self.git("status", "--porcelain")[1]:
            raise ServiceError("Repository has uncommitted changes; inspect it before retrying")
        git_dir = Path(self.git("rev-parse", "--absolute-git-dir")[1])
        for marker in ("rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD"):
            if (git_dir / marker).exists():
                raise ServiceError("Repository has an unfinished Git operation")

    def check_repo_layout(self):
        code, sparse = self.git("config", "--bool", "--get", "core.sparseCheckout", check=False)
        if code not in (0, 1):
            raise ServiceError("Cannot read sparse-checkout configuration")
        enabled = not code and sparse == "true"
        if self.repo_mode == "full":
            if enabled:
                raise ServiceError("Mac/full mode requires a full working tree; sparse checkout is enabled")
            return
        filter_code, object_filter = self.git("config", "--get", "remote.origin.partialclonefilter", check=False)
        promisor_code, promisor = self.git("config", "--bool", "--get", "remote.origin.promisor", check=False)
        if not enabled or filter_code or object_filter != "blob:none" or promisor_code or promisor != "true":
            raise ServiceError("Sparse mode requires a partial clone with blob:none and sparse checkout. "
                               "Use init-repo on a fresh path; an existing full clone is not converted or deleted")
        paths = self.git("sparse-checkout", "list")[1].splitlines()
        cone_code, cone = self.git("config", "--bool", "--get", "core.sparseCheckoutCone", check=False)
        if paths != [self.remote_path.as_posix()] or cone_code or cone != "true":
            raise ServiceError("Sparse checkout must include exactly this machine's repo_path")
        code, include = self.git("config", "--get", "lfs.fetchinclude", check=False)
        exclude_code, exclude = self.git("config", "--get", "lfs.fetchexclude", check=False)
        if code or include != self.remote_path.as_posix() + "/**" or exclude_code not in (0, 1) or exclude:
            raise ServiceError("Sparse LFS download scope must match this machine's repo_path")

    def pending(self):
        return int(self.git("rev-list", "--count", f"origin/{self.args.branch}..HEAD")[1])

    def safe_destination(self, relative):
        target = self.repo / self.remote_path / relative
        if any(is_link(path) for path in (target, *target.parents)):
            raise ServiceError(f"Destination contains a symlink: {target}")
        return target

    def integrate(self):
        self.git("fetch", "origin", self.args.branch)
        remote = f"origin/{self.args.branch}"
        merges = self.git("rev-list", "--min-parents=2", f"{remote}..HEAD")[1]
        operation = "merge" if self.direct and merges else "rebase"
        arguments = ("merge", "--no-edit", remote) if operation == "merge" else ("rebase", remote)
        code, output = self.git(*arguments, check=False)
        if code:
            self.git(operation, "--abort", check=False)
            (self.state / "BLOCKED").write_text(f"{stamp()} {operation.title()} failed; inspect commits and resolve manually.\n{output}\n", encoding="utf-8")
            raise ServiceError(f"{operation.title()} failed. Sync is blocked; resolve manually, then use unblock")

    def sync(self, dry_run=False):
        if self.direct:
            return self.sync_direct(dry_run)
        if not self.source.is_dir():
            raise ServiceError(f"Archive directory not found: {self.source}")
        if dry_run:
            files = self.inventory(self.source)
            print(f"Preview only: {len(files)} archive files; destination {self.args.repo_path}")
            print("No network, commits, state files or background tasks were created.")
            return
        with self.lock("sync"):
            if not self.network_allowed():
                return
            log = self.state / "logs/sync.log"
            if log.exists() and log.stat().st_size > 5 * 1024 * 1024:
                for number in (2, 1):
                    old = Path(f"{log}.{number}")
                    if old.exists():
                        old.replace(Path(f"{log}.{number + 1}"))
                log.replace(Path(f"{log}.1"))
            self.record(last_attempt=stamp())
            try:
                if (self.state / "BLOCKED").exists():
                    raise ServiceError("Sync is blocked; inspect BLOCKED and use unblock after resolving")
                self.initialize_repo()
                self.check_repo()
                # Never publish arbitrary commits introduced manually into the dedicated clone.
                names = [name for name in self.git("diff", "--name-only", "-z", f"origin/{self.args.branch}...HEAD")[1].split("\0") if name]
                prefix = self.remote_path.as_posix() + "/"
                if any(not name.startswith(prefix) for name in names):
                    raise ServiceError("Pending commits include files outside this machine's directory")
                if self.inventory(self.source):
                    with tempfile.TemporaryDirectory(prefix="snapshot-", dir=self.state) as temporary:
                        snapshot = Path(temporary)
                        self.snapshot(snapshot)
                        # File deletion is deliberately not propagated.
                        for relative in self.inventory(snapshot):
                            target = self.safe_destination(relative)
                            target.parent.mkdir(parents=True, exist_ok=True)
                            shutil.copyfile(snapshot / relative, target)
                        self.git("add", "--", self.remote_path.as_posix())
                        changed = self.git("diff", "--cached", "--quiet", check=False)[0]
                        if changed not in (0, 1):
                            raise ServiceError("Cannot inspect staged changes")
                        if changed:
                            self.git("-c", f"user.name={self.args.git_name}", "-c", f"user.email={self.args.git_email}",
                                     "commit", "-m", f"chore(archive): 更新 {self.remote_path.as_posix()} 归档", "--", self.remote_path.as_posix())
                if not self.pull_on_idle and not self.pending():
                    self.record(result="unchanged", sync_action="skipped", pending_commits=0, last_error=None)
                    print("Unchanged; idle sync skipped, no fetch or push.")
                    return
                self.integrate()
                self.record(last_pull=stamp())
                uploading = bool(self.pending())
                if uploading:
                    self.push_with_retry()
                self.finish_network(True)
                self.record(result="success", sync_action="push" if uploading else "pull-only",
                            last_success=stamp(), pending_commits=self.pending(), last_error=None)
                print("Git sync complete." if uploading else "Remote integrated; no upload needed.")
            except Exception as error:
                self.finish_network(False)
                self.record(result="failed", last_error=str(error))
                raise

    def verify_direct_pending(self):
        code, _ = self.git("rev-parse", "--verify", f"refs/remotes/origin/{self.args.branch}", check=False)
        if code:
            raise ServiceError("Cached origin branch is missing; fetch it manually before automatic sync")
        commits = self.git("rev-list", f"origin/{self.args.branch}..HEAD")[1].splitlines()
        marker = f"Waylog-Service-Archive: {self.remote_path.as_posix()}"
        prefix = self.remote_path.as_posix() + "/"
        for commit in commits:
            parents = self.git("show", "-s", "--format=%P", commit)[1].split()
            if len(parents) == 2:
                # Accept only a merge of already-published history that preserves
                # this device's archive exactly and adds no other local edits.
                remote = f"origin/{self.args.branch}"
                ancestor = self.git("merge-base", "--is-ancestor", parents[1], remote, check=False)[0]
                own_changed = self.git("diff", "--quiet", parents[0], commit, "--",
                                       self.remote_path.as_posix(), check=False)[0]
                local_names = [name for name in self.git("diff", "--name-only", "-z",
                                                        parents[1], commit)[1].split("\0") if name]
                if ancestor == 0 and own_changed == 0 and all(
                        name.startswith(prefix) for name in local_names):
                    continue
                raise ServiceError("Merge includes unpublished history or additional edits; reconcile it manually first")
            message = self.git("show", "-s", "--format=%B", commit)[1].splitlines()
            names = [name for name in self.git("diff-tree", "--root", "--no-commit-id", "--no-renames",
                                              "--name-only", "-r", "-z", commit)[1].split("\0") if name]
            if len(parents) != 1 or marker not in message or any(
                    not name.startswith(prefix) and name != ".gitattributes" for name in names):
                raise ServiceError("Unpushed commits not created by this archive service; push/reconcile them manually first")
            if ".gitattributes" in names:
                self.verify_lfs_attributes(commit, parents[0])

    def verify_lfs_attributes(self, commit, parent):
        def lines(ref):
            code, body = self.git("show", ref + ":.gitattributes", check=False)
            if code:
                if "does not exist in" in body or "exists on disk, but not in" in body:
                    return Counter()
                raise ServiceError("Cannot inspect LFS attribute changes")
            return Counter(line for line in body.splitlines() if line.strip())
        before, after = lines(parent), lines(commit)
        prefix = self.remote_path.as_posix() + "/"
        for line in list((after - before).elements()) + list((before - after).elements()):
            fields = shlex.split(line)
            if (len(fields) != 5 or not fields[0].startswith(prefix)
                    or ".." in Path(fields[0]).parts
                    or fields[1:] != ["filter=lfs", "diff=lfs", "merge=lfs", "-text"]):
                raise ServiceError("Pending .gitattributes edits must only add/remove this device's LFS rules")

    def sync_direct(self, dry_run=False):
        if not self.repo.is_dir() or (self.source.exists() and not self.source.is_dir()):
            raise ServiceError("Existing repository missing or archive path is not a directory; no clone will be created")
        if dry_run:
            self.check_repo(allow_archive=True)
            self.verify_direct_pending()
            print(f"Direct preview: {len(self.inventory(self.source))} archive files in {self.repo}")
            print("No network, staging, commits, state files or background tasks were created.")
            return
        with self.lock("sync"):
            if not self.network_allowed():
                return
            self.record(last_attempt=stamp(), git_repo=str(self.repo), mode="direct")
            try:
                with self.archive_access():
                    if (self.state / "BLOCKED").exists():
                        raise ServiceError("Sync is blocked; resolve manually, then use unblock")
                    self.check_repo(allow_archive=True)
                    self.verify_direct_pending()
                    files = self.inventory(self.source)
                    try:
                        self.validate_snapshot(self.source)
                    except (OSError, ValueError) as error:
                        raise ServiceError(f"Archive/index validation failed: {error}") from error
                    if files != self.inventory(self.source):
                        raise ServiceError("Archive changed outside this script; skipping sync")
                    prefix = self.remote_path.as_posix() + "/"
                    allowed = {prefix + Path(name).as_posix() for name in files}
                    listed = self.git("ls-files", "--cached", "--others", "--exclude-standard", "-z",
                                      "--", self.remote_path.as_posix())[1].split("\0")
                    candidates = sorted({name for name in listed if name in allowed})
                    if candidates:
                        with tempfile.NamedTemporaryFile(dir=self.state, prefix="pathspec-", delete=False) as handle:
                            pathspec = Path(handle.name)
                            handle.write(b"".join(name.encode("utf-8") + b"\0" for name in candidates))
                        try:
                            self.git("--literal-pathspecs", "add", f"--pathspec-from-file={pathspec}", "--pathspec-file-nul")
                        finally:
                            pathspec.unlink(missing_ok=True)
                    changed = self.git("diff", "--cached", "--quiet", check=False)[0]
                    if changed not in (0, 1):
                        raise ServiceError("Cannot inspect staged changes")
                    if changed:
                        self.git("-c", f"user.name={self.args.git_name}", "-c", f"user.email={self.args.git_email}",
                                 "commit", "-m", f"chore(archive): 更新 {self.remote_path.as_posix()} 归档\n\n"
                                 f"Waylog-Service-Archive: {self.remote_path.as_posix()}")
                    self.check_repo()  # Refuse unknown files/changes in the archive before touching the working tree.
                    self.verify_direct_pending()
                    if not self.pull_on_idle and not self.pending():
                        self.record(result="unchanged", sync_action="skipped", pending_commits=0, last_error=None)
                        print("Unchanged; idle sync skipped, no fetch or push.")
                        return
                    self.integrate()
                    self.record(last_pull=stamp())
                    self.check_repo()
                    self.verify_direct_pending()
                    uploading = bool(self.pending())
                    if uploading:
                        self.push_with_retry()
                    self.finish_network(True)
                    self.record(result="success", sync_action="push" if uploading else "pull-only",
                                last_success=stamp(), pending_commits=self.pending(), last_error=None)
                    print("Direct Git sync complete." if uploading else "Remote integrated; no upload needed.")
            except Exception as error:
                self.finish_network(False)
                self.record(result="failed", last_error=str(error))
                raise

    def init_repo(self):
        """Explicit repository preparation only; never starts watch or pushes."""
        if not self.direct:
            raise ServiceError("init-repo requires an explicit git_repo")
        with self.lock("sync"):
            if self.repo.exists():
                self.check_repo(allow_archive=True)
                self.verify_direct_pending()
                print("Existing repository already matches configuration; no changes or network access.")
                return
            if not self.network_allowed():
                return
            temporary = self.repo.with_name(f".{self.repo.name}.waylog-initializing")
            if temporary.exists():
                raise ServiceError(f"Incomplete clone preserved at {temporary}; inspect it before trying again")
            self.repo.parent.mkdir(parents=True, exist_ok=True)
            options = ["--single-branch", "--branch", self.args.branch, "--no-tags", "--no-checkout"]
            if self.repo_mode == "sparse":
                options += ["--filter=blob:none", "--sparse"]
            try:
                self.begin_network()
                _, output = run([self.git_path, "-c", "lfs.transfer.maxretries=1", "-c", "lfs.ssh.retries=1",
                                 "clone", *options, self.args.remote, str(temporary)],
                                timeout=self.args.timeout, env=dict(self.env, GIT_LFS_SKIP_SMUDGE="1"))
                if self.repo_mode == "sparse" and any(warning in output.lower() for warning in (
                        "filtering not recognized by server", "--filter is ignored")):
                    raise ServiceError("Remote ignored partial-clone filtering; refusing to continue as a sparse clone")
                def local_git(*arguments):
                    return run([self.git_path, "-C", str(temporary), "-c", "lfs.transfer.maxretries=1",
                                "-c", "lfs.ssh.retries=1", "-c", "lfs.transfer.maxverifies=1", *arguments],
                               timeout=self.args.timeout, env=self.env)
                local_git("lfs", "install", "--local")
                if self.repo_mode == "sparse":
                    local_git("config", "lfs.fetchinclude", self.remote_path.as_posix() + "/**")
                    local_git("config", "lfs.fetchexclude", "")
                    local_git("sparse-checkout", "set", "--cone", "--skip-checks", self.remote_path.as_posix())
                local_git("checkout", self.args.branch)
                if self.repo.exists():
                    raise ServiceError("Destination appeared during clone; refusing to replace it")
                temporary.rename(self.repo)
                self.check_repo()
                self.verify_direct_pending()
                self.finish_network(True)
                self.record(result="initialized", git_repo=str(self.repo), repo_mode=self.repo_mode)
                print("Repository prepared. No watch, background task, commit or push was started.")
            except Exception as error:
                self.finish_network(False)
                self.record(result="failed", last_error=f"{error}; incomplete clone, if any, retained at {temporary}")
                raise

    def initialize_repo(self):
        if self.direct:
            raise ServiceError("Direct mode never clones or initializes a repository")
        if self.repo.exists():
            return
        # LFS pointer files stay unexpanded during clone. The normal pre-push hook uploads new LFS objects.
        run([self.git_path, "lfs", "version"], env=self.env)
        temporary = self.state / "repo-initializing"
        if temporary.exists():
            raise ServiceError(f"Incomplete initialization exists: {temporary}; inspect it manually")
        try:
            self.begin_network()
            run([self.git_path, "-c", "lfs.transfer.maxretries=1", "-c", "lfs.ssh.retries=1",
                 "clone", "--single-branch", "--branch", self.args.branch,
                 self.args.remote, str(temporary)], timeout=self.args.timeout, env=self.env)
            run([self.git_path, "-C", str(temporary), "lfs", "install", "--local"], env=self.env)
            temporary.rename(self.repo)
        except Exception:
            # This path was created solely by this attempt, not an existing checkout.
            if temporary.exists():
                shutil.rmtree(temporary)
            raise

    def doctor(self):
        if self.direct and not self.repo.is_dir():
            raise ServiceError(f"Existing repository missing: {self.repo}; no clone will be created")
        if not self.source.is_dir() and (not self.direct or self.source.exists()):
            raise ServiceError(f"Archive directory missing: {self.source}")
        if not Path(self.args.waylog).is_file():
            raise ServiceError(f"Waylog binary missing: {self.args.waylog}")
        print(run([self.args.waylog, "--version"])[1])
        print(run([self.git_path, "--version"])[1])
        print(run([self.git_path, "lfs", "version"])[1])
        self.check_watch_support()
        if sys.platform == "darwin":
            self.macos_watch_command()
        if not Path(self.ssh_path).is_file():
            raise ServiceError(f"SSH executable missing: {self.ssh_path}")
        if self.repo.exists():
            self.check_repo(allow_archive=self.direct)
            if self.direct:
                self.verify_direct_pending()
            print(f"Pending commits: {self.pending()}")
        print(f"Archive: {self.source}\nRemote: {self.args.remote}\nBranch: {self.args.branch}")
        print("Local checks passed. Background SSH authentication has NOT been tested.")

    def command_arguments(self, command):
        config_path = getattr(self.args, "config_path", None)
        if config_path:
            arguments = [sys.executable, str(Path(__file__).resolve()), "--config", config_path]
            # Pin repository identity/paths for both workers. Changing these requires stop/start;
            # pull policy and retry limits are read afresh by each scheduled sync process.
            pinned = dict(archive_dir=str(self.source), git_repo=str(self.repo) if self.direct else None,
                          repo_path=self.remote_path.as_posix(), repo_mode=self.repo_mode,
                          state_dir=str(self.state), remote=self.args.remote, branch=self.args.branch,
                          waylog=self.args.waylog, git=self.git_path, ssh=self.ssh_path,
                          interval=self.args.interval, watch_interval=self.watch_interval)
            pinned.update(getattr(self.args, "config_overrides", {}))
            for name, value in pinned.items():
                if name == "git_repo" and value is None:
                    arguments.append("--isolated")
                elif name in ("pull_on_idle", "no_browser"):
                    arguments.append(("--pull-on-idle" if value else "--no-pull-on-idle")
                                     if name == "pull_on_idle" else ("--no-browser" if value else "--with-browser"))
                elif isinstance(value, list):
                    arguments += ["--" + name.replace("_", "-"), *map(str, value)]
                elif value is not None:
                    arguments += ["--" + name.replace("_", "-"), str(value)]
            return arguments + [command]
        return [sys.executable, str(Path(__file__).resolve()), "--archive-dir", str(self.source),
                "--state-dir", str(self.state), "--remote", self.args.remote,
                "--repo-path", str(self.remote_path), "--branch", self.args.branch,
                "--interval", str(self.args.interval), "--timeout", str(self.args.timeout),
                "--waylog", self.args.waylog, "--git-name", self.args.git_name,
                "--git-email", self.args.git_email,
                "--git", self.git_path, "--ssh", self.ssh_path,
                "--repo-mode", self.repo_mode, "--watch-interval", str(self.watch_interval),
                "--pull-on-idle" if self.pull_on_idle else "--no-pull-on-idle",
                *(["--git-repo", str(self.repo)] if self.direct else ["--isolated"]),
                *(["--no-browser"] if self.args.no_browser else []), command]

    def launch(self, action):
        if sys.platform != "darwin":
            raise ServiceError("start/stop require macOS launchd")
        domain = f"gui/{os.getuid()}"
        agents = Path.home() / "Library/LaunchAgents"
        for kind in ("watch", "sync"):
            label = f"local.waylog-service.{kind}"
            path = agents / f"{label}.plist"
            active = run(["launchctl", "print", f"{domain}/{label}"], check=False)[0] == 0
            if action == "stop":
                if active:
                    run(["launchctl", "bootout", f"{domain}/{label}"])
                if path.exists():
                    path.unlink()
                print(f"Stopped {kind}; data and pending commits preserved.")
                continue
            if active:
                print(f"{kind} already loaded; stop/start to apply configuration changes.")
                continue
            agents.mkdir(parents=True, exist_ok=True)
            logs = self.state / "logs"
            logs.mkdir(parents=True, exist_ok=True)
            arguments = self.macos_watch_command() if kind == "watch" else self.command_arguments(kind)
            payload = {"Label": label, "ProgramArguments": arguments,
                       "WorkingDirectory": str(self.state), "RunAtLoad": True,
                       "EnvironmentVariables": {"PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")},
                       "StandardOutPath": str(logs / f"{kind}.log"),
                       "StandardErrorPath": str(logs / f"{kind}.log")}
            if kind == "watch":
                # No interpreter/shell wrapper: macOS checks this native collector's identity.
                # Normal output rotates in Waylog; launchd captures only startup failures.
                payload.update(KeepAlive=True, ThrottleInterval=30, ExitTimeOut=15,
                               StandardOutPath=str(logs / "watch-launch.log"),
                               StandardErrorPath=str(logs / "watch-launch.log"))
            else:
                if self.args.interval == 3600:
                    # Calendar scheduling coalesces missed hours into one run on wake.
                    payload["StartCalendarInterval"] = {"Minute": 0}
                else:
                    payload["StartInterval"] = self.args.interval
            path.write_bytes(plistlib.dumps(payload))
            run(["launchctl", "bootstrap", domain, str(path)])
            print(f"Started {kind}.")

    def check_watch_support(self):
        if not self.direct:
            return
        help_text = run([self.args.waylog, "watch", "--help"], timeout=self.args.timeout)[1]
        required = ["--archive-lock-file", "--git-dir"]
        if self.watch_interval != 30:
            required.append("--interval-seconds")
        if not all(option in help_text for option in required):
            raise ServiceError("Configured Waylog lacks native watch/Git coordination support. "
                               "Build the current source for this computer and update the configured Waylog binary; "
                               "no export fallback or background task was started")

    def watch_command(self):
        command = [self.args.waylog, "--verbose", "watch", "--archive-dir", str(self.source)]
        if self.direct:
            self.check_watch_support()
            git_dir = self.git("rev-parse", "--absolute-git-dir")[1]
            command += ["--archive-lock-file", str(self.state / "archive-access.lock"),
                        "--git-dir", git_dir]
        if self.args.no_browser:
            command.append("--no-browser")
        if self.watch_interval != 30:
            command += ["--interval-seconds", str(self.watch_interval)]
        return command

    def macos_watch_command(self):
        help_text = run([self.args.waylog, "watch", "--help"], timeout=self.args.timeout)[1]
        if not all(option in help_text for option in ("--instance-lock-file", "--service-log-file")):
            raise ServiceError("Configured Waylog lacks direct macOS background support. "
                               "Install the current build and update the configuration first")
        return self.watch_command() + ["--instance-lock-file", str(self.state / "watch.lock"),
                                      "--service-log-file", str(self.state / "logs/watch-output.log")]

    def watch(self, restart=False):
        with self.lock("watch"):
            command = self.watch_command()
            # Keep the advisory lock in this supervisor for the child's lifetime.
            logs = self.state / "logs"
            logs.mkdir(exist_ok=True)
            logger = logging.getLogger("watch")
            logger.setLevel(logging.INFO)
            handler = RotatingFileHandler(logs / "watch-output.log", maxBytes=5 * 1024 * 1024, backupCount=3, encoding="utf-8")
            handler.setFormatter(logging.Formatter("%(asctime)s %(message)s"))
            logger.addHandler(handler)
            # Keep Waylog's own verbose daily files outside the synced archive.
            for path in (self.state / ".waylog/logs").glob("waylog.log.*"):
                if path.is_file() and time.time() - path.stat().st_mtime > 7 * 86400:
                    path.unlink()
            previous_handlers = {kind: signal.getsignal(kind) for kind in (signal.SIGTERM, signal.SIGINT)}
            try:
                while not CANCELLED.is_set():
                    child = subprocess.Popen(command, cwd=self.state, stdout=subprocess.PIPE,
                                             stderr=subprocess.STDOUT, text=True, encoding="utf-8",
                                             errors="replace", **process_options())
                    timer = None
                    def stop(signum, frame):
                        nonlocal timer
                        CANCELLED.set()
                        if child.poll() is None:
                            if os.name == "nt":
                                terminate_tree(child)
                            else:
                                os.killpg(child.pid, signal.SIGINT)
                                if timer is None:
                                    timer = threading.Timer(10, lambda process=child: terminate_tree(process, force=True))
                                    timer.daemon = True
                                    timer.start()
                    signal.signal(signal.SIGTERM, stop)
                    signal.signal(signal.SIGINT, stop)
                    def monitor_stop():
                        if os.name != "nt":
                            return
                        while child.poll() is None:
                            if CANCELLED.wait(0.5):
                                terminate_tree(child)
                                return
                    monitor = threading.Thread(target=monitor_stop, daemon=True)
                    monitor.start()
                    for line in child.stdout:
                        logger.info(line.rstrip())
                    code = child.wait()
                    child.stdout.close()
                    monitor.join(timeout=1)
                    if timer:
                        timer.cancel()
                    if not restart or CANCELLED.is_set():
                        return 0 if CANCELLED.is_set() else code
                    logger.warning("watch exited (%s); restarting in 30 seconds", code)
                    CANCELLED.wait(30)
                return 0
            finally:
                for kind, previous in previous_handlers.items():
                    signal.signal(kind, previous)
                logger.removeHandler(handler)
                handler.close()

    def worker(self, kind):
        if os.name != "nt":
            raise ServiceError("Windows worker commands require Windows")
        logs = self.state / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        logger = logging.getLogger(kind + "-worker")
        logger.setLevel(logging.INFO)
        logger.addHandler(RotatingFileHandler(logs / f"{kind}-worker.log", maxBytes=5 * 1024 * 1024,
                                             backupCount=3, encoding="utf-8"))
        class LogStream:
            def write(self, text):
                if text.strip():
                    logger.info("%s %s", stamp(), text.rstrip())
            def flush(self):
                pass
        def monitor():
            while not CANCELLED.wait(0.5):
                if (self.state / f"{kind}.stop").exists():
                    CANCELLED.set()
        threading.Thread(target=monitor, daemon=True).start()
        with contextlib.redirect_stdout(LogStream()), contextlib.redirect_stderr(LogStream()):
            try:
                job = windows_job()
                if (self.state / f"{kind}.stop").exists():
                    return 0
                if kind == "watch":
                    return self.watch(restart=True)
                self.sync()
                return 0
            except (ServiceError, OSError, ValueError) as error:
                print(f"ERROR: {error}")
                return 1

    def status(self):
        path = self.state / "status.json"
        print(path.read_text(encoding="utf-8") if path.exists() else "No sync has run.")
        if (self.state / "network-state.json").exists():
            print("Network retry policy:")
            print(json.dumps(self.network_state(), ensure_ascii=False, indent=2))
        if (self.state / "BLOCKED").exists():
            print((self.state / "BLOCKED").read_text(encoding="utf-8"))
        if self.repo.exists():
            print(f"Pending commits (cached remote): {self.pending()}")
        if sys.platform == "darwin":
            for kind in ("watch", "sync"):
                code, output = run(["launchctl", "print", f"gui/{os.getuid()}/local.waylog-service.{kind}"], check=False)
                details = [line.strip() for line in output.splitlines()
                           if line.strip().startswith(("state =", "pid =", "last exit code ="))]
                print(f"{kind}: {'; '.join(details) if not code else 'not loaded'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", help="JSON configuration; omitted fields use platform defaults")
    for name in ("archive_dir", "remote", "repo_path", "branch", "state_dir", "waylog",
                 "git", "ssh", "python", "git_name", "git_email"):
        parser.add_argument("--" + name.replace("_", "-"), default=argparse.SUPPRESS)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--git-repo", default=argparse.SUPPRESS)
    mode.add_argument("--isolated", action="store_true", default=argparse.SUPPRESS,
                      help="Legacy clone mode; does not refresh the source repository")
    parser.add_argument("--repo-mode", choices=("full", "sparse"), default=argparse.SUPPRESS)
    parser.add_argument("--pull-on-idle", action=argparse.BooleanOptionalAction, default=argparse.SUPPRESS)
    for name in ("interval", "timeout", "watch_interval", "retry_max_failures", "minimum_network_interval"):
        parser.add_argument("--" + name.replace("_", "-"), type=int, default=argparse.SUPPRESS)
    parser.add_argument("--retry-delays", type=int, nargs="+", default=argparse.SUPPRESS)
    browser = parser.add_mutually_exclusive_group()
    browser.add_argument("--no-browser", action="store_true", default=argparse.SUPPRESS)
    browser.add_argument("--with-browser", dest="no_browser", action="store_false", default=argparse.SUPPRESS)
    parser.add_argument("--dry-run", action="store_true", help="sync only; read-only preview")
    parser.add_argument("command", choices=("start", "stop", "status", "sync", "doctor", "logs", "watch",
                                           "unblock", "resume-sync", "init-repo", "config",
                                           "watch-worker", "sync-worker", "task-spec"))
    parsed = vars(parser.parse_args())
    command, dry_run = parsed.pop("command"), parsed.pop("dry_run")
    config_file = parsed.pop("config")
    isolated = parsed.pop("isolated", False)
    if isolated:
        parsed.update(git_repo=None, repo_mode="full")
    values, config_path = load_configuration(config_file, parsed)
    args = argparse.Namespace(**values, command=command, dry_run=dry_run, isolated=isolated,
                              config_path=config_path, config_overrides=parsed)
    if args.dry_run and args.command != "sync":
        parser.error("--dry-run is only supported for sync")
    if args.command in ("start", "task-spec") and not config_path:
        raise ServiceError("Create a local JSON configuration from the example before installing tasks")
    service = Service(args)
    if args.command == "config":
        print(json.dumps(dict(values, config_path=config_path), ensure_ascii=False, indent=2))
    elif args.command == "init-repo":
        service.init_repo()
    elif args.command == "sync":
        service.sync(args.dry_run)
    elif args.command == "start":
        service.doctor()
        service.launch("start")
    elif args.command == "stop":
        service.launch("stop")
    elif args.command == "watch":
        sys.exit(service.watch())
    elif args.command.endswith("-worker"):
        sys.exit(service.worker(args.command.split("-")[0]))
    elif args.command == "task-spec":
        executable = Path(sys.executable).with_name("pythonw.exe") if os.name == "nt" else Path(sys.executable)
        if not executable.is_file():
            raise ServiceError("pythonw.exe is required beside python.exe for windowless Windows tasks")
        print(json.dumps({"executable": str(executable),
                          "watch": subprocess.list2cmdline(service.command_arguments("watch-worker")[1:]),
                          "sync": subprocess.list2cmdline(service.command_arguments("sync-worker")[1:])},
                         ensure_ascii=True))
    elif args.command == "resume-sync":
        service.resume_sync()
    elif args.command in ("doctor", "status"):
        getattr(service, args.command)()
    elif args.command == "unblock":
        with service.lock("sync"):
            service.check_repo(allow_archive=service.direct)
            (service.state / "BLOCKED").unlink(missing_ok=True)
            print("Unblocked. No sync or push was performed.")
    else:
        for path in sorted((service.state / "logs").glob("*.log")):
            print(f"--- {path} ---")
            with path.open(encoding="utf-8", errors="replace") as handle:
                from collections import deque
                print("".join(deque(handle, maxlen=80)))


if __name__ == "__main__":
    try:
        main()
    except (ServiceError, OSError, ValueError) as error:
        print(f"{stamp()} ERROR: {error}", file=sys.stderr)
        sys.exit(1)
