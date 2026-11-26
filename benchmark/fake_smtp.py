"""Accept-all fake SMTP server for the memory benchmark (stdlib only).

Why not mailcrab:1025 by default? Mailcrab advertises no AUTH extension;
since ADR 003 the worker supports no-AUTH against loopback with
``PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true`` (plus ``EMAIL_USE_TLS=false`` +
``PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true``), so mailcrab would drain. This
server advertises ``AUTH PLAIN LOGIN``, accepts any credentials, and also
accepts unauthenticated senders — so the minimal Python baseline (no login)
and the Rust worker (login) both drain against it, and the benchmark keeps
exercising the AUTH path.

Usage: ``.venv/bin/python benchmark/fake_smtp.py --port 1026 [--mailbox DIR]``
Prints ``READY <port>`` on stdout when listening. Stores one ``*.eml`` per
message when ``--mailbox`` is given (lets the benchmark prove delivery).
"""

from __future__ import annotations

import argparse
import os
import socketserver
import threading

counter = 0
counter_lock = threading.Lock()


class Handler(socketserver.StreamRequestHandler):
    def reply(self, msg: str) -> None:
        self.wfile.write(msg.encode() + b'\r\n')

    def readline(self) -> str:
        return self.rfile.readline().decode('utf-8', 'replace').strip()

    def handle(self) -> None:
        global counter
        mailbox = self.server.mailbox
        self.reply('220 bench fake-smtp ready')
        mail_from, rcpts = '', []
        while True:
            line = self.readline()
            if not line:
                return
            verb, _, arg = line.partition(' ')
            verb = verb.upper()
            if verb in ('EHLO', 'HELO'):
                self.reply('250-HELLO')
                self.reply('250 AUTH PLAIN LOGIN')
            elif verb == 'AUTH':
                mech, _, init = arg.partition(' ')
                if mech.upper() == 'PLAIN' and init:
                    self.reply('235 ok')
                elif mech.upper() == 'PLAIN':
                    self.reply('334 ')
                    self.readline()
                    self.reply('235 ok')
                elif mech.upper() == 'LOGIN':
                    self.reply('334 VXNlcm5hbWU6')
                    self.readline()
                    self.reply('334 UGFzc3dvcmQ6')
                    self.readline()
                    self.reply('235 ok')
                else:
                    self.reply('504 unsupported')
            elif verb == 'MAIL':
                mail_from = arg
                rcpts = []
                self.reply('250 ok')
            elif verb == 'RCPT':
                rcpts.append(arg)
                self.reply('250 ok')
            elif verb == 'DATA':
                if not rcpts:
                    self.reply('503 need RCPT')
                    continue
                self.reply('354 end with .')
                data = []
                while True:
                    chunk = self.rfile.readline().decode('utf-8', 'replace')
                    if chunk in ('.\r\n', '.\n'):
                        break
                    data.append(chunk)
                with counter_lock:
                    counter += 1
                    n = counter
                if mailbox:
                    with open(os.path.join(mailbox, f'{n:04d}.eml'), 'w') as fh:
                        fh.write(f'From: {mail_from}\nTo: {", ".join(rcpts)}\n\n')
                        fh.writelines(data)
                self.reply('250 queued')
            elif verb == 'RSET':
                mail_from, rcpts = '', []
                self.reply('250 ok')
            elif verb == 'NOOP':
                self.reply('250 ok')
            elif verb == 'QUIT':
                self.reply('221 bye')
                return
            else:
                self.reply('502 unsupported')


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', default='127.0.0.1')
    parser.add_argument('--port', type=int, default=1026)
    parser.add_argument('--mailbox', default=None)
    args = parser.parse_args()
    if args.mailbox:
        os.makedirs(args.mailbox, exist_ok=True)
    server = socketserver.ThreadingTCPServer((args.host, args.port), Handler)
    server.mailbox = args.mailbox
    server.daemon_threads = True
    print(f'READY {args.port}', flush=True)
    server.serve_forever()


if __name__ == '__main__':
    main()
