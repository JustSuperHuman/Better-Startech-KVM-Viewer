"""Explicit-input hardware checks over the same private IPC as the viewer.
Run with Python 3 -I. Does not read or overwrite the local clipboard.
"""
import argparse
import collections
import json
import os
import pathlib
import struct
import subprocess
import threading
import time
import zlib


def png(path, frame):
    width, height, rgba = frame
    def chunk(tag, data):
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', zlib.crc32(tag + data) & 0xffffffff)
    rows = b''.join(b'\0' + rgba[y * width * 4:(y + 1) * width * 4] for y in range(height))
    data = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>2I5B', width, height, 8, 6, 0, 0, 0))
    data += chunk(b'IDAT', zlib.compress(rows, 6)) + chunk(b'IEND', b'')
    with path.open('xb') as output:
        output.write(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', type=pathlib.Path, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--stage', choices=['comment', 'compare', 'enter', 'keypad-enter', 'tty', 'od-session', 'od-prepare', 'od-data', 'cancel', 'cleanup'], required=True)
    parser.add_argument('--allow-input', action='store_true', help='Required: sends keystrokes to the connected target')
    args = parser.parse_args()
    if not args.allow_input:
        parser.error('This test requires explicit --allow-input')
    if args.output.exists():
        parser.error('Refusing to overwrite an existing screenshot')
    environment = dict(os.environ, CART_TRACE_IO='1')
    process = subprocess.Popen([str(args.helper), '--serve'], env=environment, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
                               cwd=str(pathlib.Path(__file__).resolve().parent.parent))
    state = {'status': {}, 'frame': None, 'error': None, 'revision': 0, 'frames': 0}
    lock = threading.Lock()
    logs = collections.deque(maxlen=40)

    def exact(count):
        result = bytearray()
        while len(result) < count:
            data = process.stdout.read(count - len(result))
            if not data:
                raise EOFError('Helper output closed')
            result.extend(data)
        return result

    def reader():
        try:
            while True:
                magic, version, kind, seq, width, height, count = struct.unpack('<4sHHIIII', exact(24))
                if magic != b'CART' or version != 1 or count > 1920 * 1600 * 4:
                    raise ValueError('Invalid helper frame header')
                payload = bytes(exact(count))
                with lock:
                    if kind == 1:
                        if not (0 < width <= 1920 and 0 < height <= 1600 and count == width * height * 4):
                            raise ValueError('Invalid frame layout')
                        state['frame'] = (width, height, payload)
                        state['frames'] += 1
                    elif kind == 2:
                        status = json.loads(payload)
                        state['status'].update(status)
                        if 'pasting' in status:
                            state['revision'] += 1
                    else:
                        raise ValueError('Unknown record kind')
        except Exception as error:
            with lock:
                state['error'] = str(error)

    def stderr():
        for line in process.stderr:
            logs.append(line.decode('utf-8', 'replace').rstrip())

    threading.Thread(target=reader, daemon=True).start()
    threading.Thread(target=stderr, daemon=True).start()

    def wait(predicate, timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with lock:
                if predicate(state):
                    return
                if state['error'] or state['status'].get('state') == 'error':
                    raise RuntimeError(state['error'] or state['status'].get('message', 'Device error'))
            time.sleep(0.02)
        raise TimeoutError('Hardware condition timed out')

    def send(kind, **fields):
        process.stdin.write((json.dumps(dict(type=kind, **fields)) + '\n').encode('utf-8'))
        process.stdin.flush()

    def paste(text, delay=150):
        with lock:
            baseline = state['revision']
        started = time.monotonic()
        send('paste', text=text, delay_ms=delay)
        wait(lambda s: s['revision'] > baseline and s['status'].get('pasting') is True, 5)
        wait(lambda s: s['revision'] > baseline and s['status'].get('pasting') is False,
             max(30, len(text) * (delay + 150) / 1000 + 10))
        print('Paste duration: %.3f seconds' % (time.monotonic() - started))
        expected = len(text.replace('\r\n', '\n').replace('\r', '\n'))
        with lock:
            if state['status'].get('paste_done') != expected:
                raise RuntimeError('Paste stopped before completion: ' + repr(state['status']))

    def key(usage, down):
        send('key', usage=usage, down=down)
        time.sleep(0.07)

    def control(usage):
        key(0xe0, True)
        key(usage, True)
        key(usage, False)
        key(0xe0, False)

    try:
        wait(lambda s: s['frame'] is not None and s['status'].get('keyboard_ready') is True)
        if args.stage == 'comment':
            control(0x06)
            control(0x18)
            control(0x0f)  # Ctrl+L clears only the terminal display, not logs or files.
            paste('# Rust viewer: AaZz 0123456789 !@#$%^&*()_+-=[]{};:\'",.<>/?\\|`~')
        elif args.stage == 'compare':
            control(0x06)
            control(0x18)
            control(0x0f)
            for usage in (4, 5, 0x1e, 0x1f, 6, 7, 0x20, 0x21):
                key(usage, True)
                key(usage, False)
            time.sleep(0.5)
            with lock:
                manual_frame = state['frame']
            png(args.output.with_name(args.output.stem + '-manual.png'), manual_frame)
            control(0x18)
            control(0x0f)
            paste('ab12cd34')
        elif args.stage == 'keypad-enter':
            control(0x06)
            control(0x18)
            control(0x0f)
            paste('echo keypad-ok')
            key(0x58, True)
            time.sleep(0.1)
            key(0x58, False)
            time.sleep(1)
        elif args.stage == 'tty':
            control(0x14)  # Ctrl+Q resumes terminal output if software flow control stopped it.
            control(0x06)
            control(0x18)
            control(0x0f)
            paste('stty -a')  # Read-only terminal settings; no configuration changes.
            control(0x0d)  # Ctrl+J tests LF independently of the Return key.
            time.sleep(1)
        elif args.stage == 'enter':
            key(0x28, True)
            time.sleep(0.1)
            key(0x28, False)
        elif args.stage == 'od-session':
            permit = args.output.with_suffix('.permit')
            if permit.exists():
                raise RuntimeError('Refusing a stale readiness permit')
            control(0x14)
            control(0x06)
            control(0x18)
            control(0x0f)
            paste('echo stream-ready; od -tx1 | cat -n\n')
            time.sleep(1.5)
            with lock:
                ready_frame = state['frame']
            ready_path = args.output.with_name(args.output.stem + '-ready.png')
            png(ready_path, ready_frame)
            print('READY_FOR_INSPECTION:', ready_path, flush=True)
            deadline = time.monotonic() + 120
            while not permit.exists() and time.monotonic() < deadline:
                time.sleep(0.1)
            if not permit.exists() or permit.read_text().strip() != 'continue':
                control(0x06)
                control(0x18)
                raise RuntimeError('Target readiness was not approved by the test controller')
            payload = '# ' + ''.join(chr(n) for n in range(32, 127)) + '\r\n\t# Paste OK\n'
            paste(payload)
            control(0x07)
            time.sleep(1.5)
            print('Expected target byte count:', len(payload.replace('\r\n', '\n')), '; final od offset (octal): 0000156')
        elif args.stage == 'od-prepare':
            control(0x06)
            control(0x18)  # Ctrl+U clears the shell input line, not a file.
            control(0x0f)
            paste('echo ready; od -tx1\n')
        elif args.stage == 'od-data':
            paste('# ' + ''.join(chr(n) for n in range(32, 127)) + '\r\n\t# Paste OK\n')
            control(0x07)  # Ctrl+D: EOF to the already-verified od process.
        elif args.stage == 'cancel':
            send('paste', text='# ' + 'X' * 200, delay_ms=100)
            wait(lambda s: s['status'].get('pasting') is True, 5)
            time.sleep(0.35)
            with lock:
                baseline = state['revision']
            send('cancel')
            wait(lambda s: s['revision'] > baseline and s['status'].get('pasting') is False, 5)
            with lock:
                if state['status'].get('paste_done', 200) >= 200:
                    raise RuntimeError('Cancellation did not interrupt the paste')
        elif args.stage == 'cleanup':
            control(0x06)  # Ctrl+C stops only a foreground test utility.
            control(0x18)
        time.sleep(0.7)
        with lock:
            frame = state['frame']
        png(args.output, frame)
        if args.stage in ('comment', 'compare', 'cancel'):
            control(0x18)
        send('release')
        print('Hardware stage completed:', args.stage, '; rendered frames received:', state['frames'])
        print('Screenshot:', args.output)
    except Exception:
        with lock:
            frame = state['frame']
            status = dict(state['status'])
        if frame is not None and not args.output.exists():
            png(args.output, frame)
        print('Last device status:', status)
        for line in logs:
            print('helper:', line)
        raise
    finally:
        try:
            send('cancel')
            send('release')
            send('quit')
            process.stdin.close()
            process.wait(timeout=8)
        except (OSError, subprocess.TimeoutExpired):
            process.kill()  # Only the child created by this test; never the vendor viewer.
            process.wait()
        for line in logs:
            if line.startswith('Bridge '):
                print(line)
        if process.returncode:
            print('Helper exited with code', process.returncode)
            for line in logs:
                print('helper:', line)


if __name__ == '__main__':
    main()
