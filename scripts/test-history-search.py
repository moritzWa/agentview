#!/usr/bin/env python3
"""Isolated PTY regression for indexed Claude/Codex search and exact resume.

Run on macOS/Linux (including the cua macOS VM):
    python3 scripts/test-history-search.py /path/to/agentview

Uses representative provider logs and mock native CLIs. No accounts, network,
real provider homes, or desktop windows are used.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time


MOCK = r'''#!/usr/bin/env python3
import json, os, socket, sys, threading, time
from pathlib import Path
root = Path(os.environ['HISTORY_TEST_ROOT'])
provider = Path(sys.argv[0]).name
args = sys.argv[1:]
if '--resume' in args or (provider == 'codex' and args[:1] == ['resume']):
    with (root / 'resumes.jsonl').open('a') as f:
        f.write(json.dumps({'provider': provider, 'args': args, 'cwd': os.getcwd()}) + '\n')
    print('NATIVE RESUMED ' + provider + ' ' + args[-1], flush=True)
    time.sleep(0.15)
elif provider == 'codex' and args[:2] == ['app-server', '--listen']:
    with (root / 'server-groups').open('a') as f:
        f.write(str(os.getpgrp()) + '\n')
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(args[2][len('unix://'):]); os.chmod(args[2][len('unix://'):], 0o600)
    server.listen()
    def handle(conn):
        with conn, conn.makefile('rwb') as stream:
            for raw in stream:
                message = json.loads(raw)
                if 'id' not in message: continue
                method = message.get('method')
                if method == 'initialize': result = {'userAgent':'mock/1'}
                elif method in ('thread/list','model/list'): result = {'data':[], 'nextCursor':None}
                elif method == 'thread/loaded/list': result = {'data':[]}
                else: result = {}
                stream.write((json.dumps({'id':message['id'],'result':result})+'\n').encode()); stream.flush()
    while True:
        conn, _ = server.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()
elif '--version' in args: print('1.99.0')
elif provider == 'claude' and args[:1] == ['agents']: print('[]')
elif '--help' in args: print('Models: sonnet, opus, haiku')
else: print('{}')
'''


def write_records(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(''.join(json.dumps(row) + '\n' for row in records))


def index_times(root):
    result = {}
    for path in (root / 'state' / 'agentview').glob('*history-search-*.db'):
        try:
            with sqlite3.connect(path) as db:
                row = db.execute("SELECT value FROM meta WHERE key='refreshed'").fetchone()
                if row:
                    result[path] = row[0]
        except sqlite3.Error:
            pass
    return result


class Dashboard:
    def __init__(self, binary, root):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 140, 0, 0))
        env = dict(os.environ, HOME=str(root), XDG_STATE_HOME=str(root / 'state'),
                   XDG_DATA_HOME=str(root / 'data'), XDG_CONFIG_HOME=str(root / 'config'),
                   CLAUDE_CONFIG_DIR=str(root / 'claude-home'), CODEX_HOME=str(root / 'codex-home'),
                   TERM='xterm-256color', HISTORY_TEST_ROOT=str(root))
        args = [str(binary), '--claude-bin', str(root / 'claude'), '--codex-bin', str(root / 'codex'),
                '--include-external', '--include-interactive', '--history-limit', '1', '--theme', 'dark']
        for provider in ['pi', 'opencode', 'cursor', 'copilot', 'antigravity', 'mistral-vibe', 'muse',
                         'qwen', 'kimi', 'omp', 'grok', 'kilo', 'openhands', 'hermes', 'mastracode', 'devin']:
            args.append('--no-host-' + provider)
        self.proc = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave,
                                     env=env, cwd=root, start_new_session=True)
        os.close(slave)
        self.raw = bytearray()

    def wait(self, token, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.05)[0]:
                try:
                    self.raw.extend(os.read(self.master, 65536))
                except OSError:
                    break
            if token.encode() in self.raw:
                return
        raise AssertionError('Missing ' + token + '\n' + self.raw.decode(errors='replace')[-9000:])

    def search_and_resume(self, query, provider, ident):
        self.raw.clear()
        os.write(self.master, b'\x07')
        self.wait('find session')
        self.raw.clear()
        start = time.monotonic()
        os.write(self.master, query.encode())
        self.wait(query, 5)
        # This word occurs only in the assistant reply, never in the title.
        self.wait('conversation marker', 5)
        elapsed = (time.monotonic() - start) * 1000
        self.raw.clear()
        os.write(self.master, b'\r')
        self.wait('NATIVE RESUMED ' + provider + ' ' + ident)
        self.wait('describe a task')
        print(json.dumps({'provider': provider, 'query': query, 'search_ms': round(elapsed, 1),
                          'exact_resume': ident}), flush=True)

    def close(self):
        if self.proc.poll() is None:
            os.write(self.master, b'\x1b')
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGTERM)
                try:
                    self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(self.proc.pid, signal.SIGKILL)
                    self.proc.wait()
        os.close(self.master)


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix='av-history-providers-') as directory:
        root = Path(directory)
        root.chmod(0o700)
        work = root / 'project'
        work.mkdir()
        claude_id = '11111111-1111-4111-8111-111111111111'
        codex_id = '22222222-2222-4222-8222-222222222222'
        claude_file = root / 'claude-home' / 'projects' / 'project' / (claude_id + '.jsonl')
        codex_file = root / 'codex-home' / 'sessions' / '2026' / '10' / '15' / 'rollout.jsonl'
        write_records(claude_file, [
            {'type':'user', 'cwd':str(work), 'message':{'content':'Investigate service behavior'}},
            {'type':'assistant', 'message':{'content':[{'type':'text', 'text':'auroraclaude conversation marker'}]}},
            {'type':'custom-title', 'customTitle':'Older Claude research'},
        ])
        os.utime(claude_file, (time.time()-86400, time.time()-86400))
        write_records(claude_file.with_name('33333333-3333-4333-8333-333333333333.jsonl'), [
            {'type':'user', 'cwd':str(work), 'message':{'content':'Newer decoy session'}},
        ])
        write_records(codex_file, [
            {'type':'session_meta','payload':{'id':codex_id,'cwd':str(work)}},
            {'type':'response_item','payload':{'type':'message','role':'user','content':[{'type':'input_text','text':'Review service behavior'}]}},
            {'type':'response_item','payload':{'type':'message','role':'assistant','content':[{'type':'output_text','text':'meridiancodex conversation marker'}]}},
        ])
        original = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in [claude_file, codex_file]}
        for provider in ['claude', 'codex']:
            path = root / provider
            path.write_text(MOCK)
            path.chmod(0o700)
        try:
            stamps = None
            for restarted in [False, True]:
                app = Dashboard(binary, root)
                try:
                    app.wait('describe a task')
                    deadline = time.monotonic() + 20
                    while len(index_times(root)) < 2 and time.monotonic() < deadline:
                        time.sleep(0.05)
                    assert len(index_times(root)) == 2, 'both indices must build before opening search'
                    if restarted:
                        assert index_times(root) == stamps, 'fresh persisted indices must not rebuild'
                    app.search_and_resume('auroraclaude', 'claude', claude_id)
                    app.search_and_resume('meridiancodex', 'codex', codex_id)
                    stamps = index_times(root)
                finally:
                    app.close()
            resumes = [json.loads(line) for line in (root / 'resumes.jsonl').read_text().splitlines()]
            assert len(resumes) == 4, resumes
            for item in resumes:
                assert Path(item['cwd']).resolve() == work.resolve(), item
                expected = ['--resume', claude_id] if item['provider'] == 'claude' else ['resume', codex_id]
                assert item['args'] == expected, item
            assert all(hashlib.sha256(p.read_bytes()).hexdigest() == digest for p, digest in original.items())
            print('PASS: both providers, body snippets, exact native resume, persistence, unchanged sources', flush=True)
        finally:
            groups = root / 'server-groups'
            if groups.exists():
                for group in set(map(int, groups.read_text().splitlines())):
                    if group > 1 and group != os.getpgrp():
                        try:
                            os.killpg(group, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                        deadline = time.monotonic() + 3
                        while time.monotonic() < deadline:
                            try:
                                os.killpg(group, 0)
                            except ProcessLookupError:
                                break
                            time.sleep(0.05)
                        else:
                            try:
                                os.killpg(group, signal.SIGKILL)
                            except ProcessLookupError:
                                pass


if __name__ == '__main__':
    main()
