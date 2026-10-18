#!/usr/bin/env python3
"""Run real Firestore emulator + explicitly fake GCS API + Cargo in one namespace.

No credentials, Google Cloud resources, or deployment. Fake GCS is only a local
contract fixture; this does not establish real Google Cloud Storage integration.
Default runs ignored Firestore tests, or supply Cargo arguments after --.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time
from urllib.parse import urlsplit, parse_qs, unquote

ROOT = Path(__file__).resolve().parents[1]
STORE = {}
LOCK = threading.Lock()
class FakeGCS(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def answer(self, status, value, mime='application/json'):
        payload = json.dumps(value).encode() if mime == 'application/json' else value
        self.send_response(status)
        self.send_header('Content-Type', mime)
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
    def do_POST(self):
        url = urlsplit(self.path)
        query = parse_qs(url.query)
        if not url.path.startswith('/upload/storage/v1/b/') or query.get('uploadType') != ['media']:
            return self.answer(400, {'error': 'unsupported fixture operation'})
        if query.get('ifGenerationMatch') != ['0']:
            return self.answer(400, {'error': 'immutable upload precondition required'})
        name = query.get('name', [''])[0]
        bucket = url.path.split('/')[5]
        length = int(self.headers.get('Content-Length', '0'))
        if length > 32 * 1024 * 1024:
            return self.answer(413, {'error': 'fixture size limit'})
        data = self.rfile.read(length)
        with LOCK:
            key = (bucket, name)
            if key in STORE:
                return self.answer(412, {'error': 'already exists'})
            generation = str(len(STORE) + 1)
            STORE[key] = (generation, data)
        return self.answer(200, {'name': name, 'generation': generation})
    def do_GET(self):
        url = urlsplit(self.path)
        query = parse_qs(url.query)
        if '/o/' not in url.path or query.get('alt') != ['media']:
            return self.answer(400, {'error': 'unsupported fixture read'})
        bucket = url.path.split('/')[4]
        name = unquote(url.path.split('/o/', 1)[1])
        with LOCK:
            item = STORE.get((bucket, name))
        if item is None or query.get('generation') != [item[0]]:
            return self.answer(404, {'error': 'object generation not found'})
        return self.answer(200, item[1], 'application/octet-stream')

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('cargo_args', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    jar = ROOT / '.local/firebase-home/.cache/firebase/emulators/cloud-firestore-emulator-v1.22.0.jar'
    if not jar.is_file():
        raise SystemExit('Official Firestore emulator JAR missing; install locally first.')
    env = os.environ.copy()
    env.update(CARGO_HOME='/workspace/shared/wipstash-toolchain/cargo', RUSTUP_HOME='/workspace/shared/wipstash-toolchain/rustup')
    env['PATH'] = env['CARGO_HOME'] + '/bin:' + env['PATH']
    env.update(ARTIFACT_TEST_FIRESTORE_URL='http://127.0.0.1:8187', FIRESTORE_EMULATOR_URL='http://127.0.0.1:8187', GCS_EMULATOR_URL='http://127.0.0.1:4443')
    gcs = http.server.ThreadingHTTPServer(('127.0.0.1', 4443), FakeGCS)
    threading.Thread(target=gcs.serve_forever, daemon=True).start()
    log_path = ROOT / '.local/firestore-contract.log'
    with log_path.open('w') as log:
        java = subprocess.Popen(['java', '-jar', str(jar), '--host=127.0.0.1', '--port=8187', '--project_id=demo-artifact-board'], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 60
            while True:
                if java.poll() is not None:
                    raise RuntimeError('Firestore emulator exited: ' + log_path.read_text()[-5000:])
                try:
                    with socket.create_connection(('127.0.0.1', 8187), timeout=.3):
                        break
                except OSError:
                    if time.monotonic() > deadline:
                        raise RuntimeError('Firestore emulator startup timed out: ' + log_path.read_text()[-5000:])
                    time.sleep(.2)
            extra = args.cargo_args
            if extra and extra[0] == '--':
                extra = extra[1:]
            command = ['cargo', 'test'] + (extra or ['firestore_emulator_transaction_contract', '--', '--ignored', '--nocapture'])
            print('Running real Firestore emulator + fake GCS contract fixtures.', flush=True)
            return subprocess.call(command, cwd=ROOT, env=env)
        finally:
            gcs.shutdown()
            java.terminate()
            try:
                java.wait(timeout=5)
            except subprocess.TimeoutExpired:
                java.kill()
                java.wait()
if __name__ == '__main__':
    raise SystemExit(main())
