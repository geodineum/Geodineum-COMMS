#!/usr/bin/env python3
"""A stand-in for the Bot API, for proofs: python3 -I fake_telegram.py <port> <spool-dir>

Every call is appended to <spool>/calls.jsonl as {bot, token_sha, method, body}; the token itself is never written.
Updates to serve are JSON files in <spool>/<bot id>/ (the digits before the colon in the token), taken in name order.
As the real API does: a second getUpdates on a bot while one is open answers 409, and a bot with a file
<spool>/<bot id>/.not_started answers 403 to sendMessage (a bot may not write first to someone who never pressed Start).
"""
import hashlib
import json
import os
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SPOOL = sys.argv[2]
lock = threading.Lock()
polling = set()
next_update = {}
next_message = [1000]


def bot_id(token):
    head = token.split(':')[0]
    return head if head.isdigit() else hashlib.sha256(token.encode()).hexdigest()[:8]


def take_updates(bot):
    d = os.path.join(SPOOL, bot)
    out = []
    if not os.path.isdir(d):
        return out
    for name in sorted(os.listdir(d)):
        if name.startswith('.'):
            continue
        path = os.path.join(d, name)
        with open(path) as f:
            update = json.load(f)
        os.unlink(path)
        with lock:
            uid = next_update.get(bot, 1)
            next_update[bot] = uid + 1
        update['update_id'] = uid
        out.append(update)
    return out


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def answer(self, status, payload):
        raw = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        try:
            self.wfile.write(raw)
        except BrokenPipeError:
            pass  # the poller was stopped while its request was open

    def body(self):
        raw = self.rfile.read(int(self.headers.get('Content-Length') or 0))
        kind = self.headers.get('Content-Type') or ''
        if 'json' in kind:
            return json.loads(raw or b'{}')
        form = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(self.path).query))
        form.update(urllib.parse.parse_qsl(raw.decode(errors='replace')))
        return form

    def handle_call(self):
        parts = urllib.parse.urlparse(self.path).path.strip('/').split('/')
        if len(parts) != 2 or not parts[0].startswith('bot'):
            return self.answer(404, {'ok': False, 'error_code': 404, 'description': 'Not Found'})
        token, method = parts[0][3:], parts[1]
        bot = bot_id(token)
        body = self.body()
        with lock, open(os.path.join(SPOOL, 'calls.jsonl'), 'a') as f:
            f.write(json.dumps({'bot': bot, 'token_sha': hashlib.sha256(token.encode()).hexdigest()[:12],
                                'method': method, 'body': body}) + '\n')
        if method == 'getUpdates':
            with lock:
                conflict = bot in polling
                polling.add(bot)
            if conflict:
                return self.answer(409, {'ok': False, 'error_code': 409,
                                         'description': 'Conflict: terminated by other getUpdates request'})
            try:
                wait = min(float(body.get('timeout', 0) or 0), 1.0)
                end = time.time() + wait
                updates = take_updates(bot)
                while not updates and time.time() < end:
                    time.sleep(0.05)
                    updates = take_updates(bot)
                return self.answer(200, {'ok': True, 'result': updates})
            finally:
                with lock:
                    polling.discard(bot)
        if method == 'getMe':
            return self.answer(200, {'ok': True, 'result': {'id': int(bot) if bot.isdigit() else 0, 'is_bot': True,
                                                            'first_name': 'proof', 'username': 'proof_' + bot + '_bot'}})
        if method == 'sendMessage':
            if os.path.exists(os.path.join(SPOOL, bot, '.not_started')):
                return self.answer(403, {'ok': False, 'error_code': 403,
                                         'description': "Forbidden: bot can't initiate conversation with a user"})
            with lock:
                next_message[0] += 1
                mid = next_message[0]
            return self.answer(200, {'ok': True, 'result': {'message_id': mid}})
        return self.answer(200, {'ok': True, 'result': True})

    do_GET = handle_call
    do_POST = handle_call


if __name__ == '__main__':
    ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
