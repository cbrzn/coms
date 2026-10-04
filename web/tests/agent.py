#!/usr/bin/env python3
"""Offline terminal fixture: record submitted input, with real bracketed paste."""
import json
import os
import pathlib
import sys
import tty

tty.setraw(sys.stdin.fileno())
role = os.environ['TMUXOR_ROLE']
record = pathlib.Path(os.environ['TMUXOR_HOME']) / f'received-{role}.jsonl'
sys.stdout.write(f'\x1b[?2004h\x1b[32m{role.upper()}\x1b[0m · fixture agent ready\r\n\r\nWaiting for your direction.\r\n> ')
sys.stdout.flush()
text = ''
escape = ''
pasting = False
while True:
    char = sys.stdin.read(1)
    if not char:
        break
    if char == '\x1b' or escape:
        escape += char
        if escape == '\x1b[200~':
            pasting = True
            escape = ''
        elif escape == '\x1b[201~':
            pasting = False
            escape = ''
        elif len(escape) > 6:
            escape = ''
        continue
    if char in '\r\n' and not pasting:
        with record.open('a') as file:
            file.write(json.dumps(text) + '\n')
        sys.stdout.write('\r\n\x1b[90mMessage received.\x1b[0m\r\n> ')
        sys.stdout.flush()
        text = ''
    elif char == '\x03':
        text = ''
    else:
        if pasting and char == '\r':
            char = '\n'
        text += char
        sys.stdout.write(char.replace('\n', '\r\n'))
        sys.stdout.flush()
