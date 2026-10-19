'use strict';
const assert = require('node:assert/strict');
const { setTimeout: delay } = require('node:timers/promises');
const origin = 'http://127.0.0.1:4317';
let cookie = '';
async function request(path, method = 'GET', data) {
 const response = await fetch(origin + path, { method, headers: { 'Content-Type': 'application/json', 'X-Artifact-Request': '1', Cookie: cookie }, ...(data === undefined ? {} : { body: JSON.stringify(data) }) });
 assert.equal(response.status, 200, 'Image smoke HTTP operation failed');
 return response;
}
(async () => {
 let health;
 for (let attempt = 0; attempt < 60; attempt++) {
  try { health = await (await request('/health')).json(); break; } catch { await delay(1000); }
 }
 assert.equal(health?.identity?.development_fixture, true, 'This smoke runner requires a disposable loopback fixture container');
 const login = await request('/dev/login', 'POST', { identity: 'owner' });
 cookie = login.headers.get('set-cookie').split(';')[0];
 for (const [kind, entrypoint, content] of [['static', 'index.html', '<!doctype html><h1>Image acceptance</h1>'], ['markdown', 'README.md', '# Image acceptance\n\nDurable review context.']]) {
  const share = await (await request('/api/shares', 'POST', { title: 'Image acceptance ' + kind, kind, policy: 'link-only', idempotency_key: 'image-share-' + kind })).json();
  const upload = await (await request('/api/shares/' + share.id + '/uploads', 'POST', {})).json();
  await request(upload.upload_url, 'PUT', { files: [{ path: entrypoint, content }] });
  const payload = { entrypoint, idempotency_key: 'image-finalize-' + kind, mutation_actor: 'auth:dev-owner' };
  const ready = await (await request('/api/uploads/' + upload.id + '/finalize', 'POST', payload)).json();
  assert.equal(ready.state, 'ready');
  const replay = await (await request('/api/uploads/' + upload.id + '/finalize', 'POST', payload)).json();
  assert.deepEqual(replay, ready);
  const html = await (await request(ready.viewer_url)).text();
  assert.ok(html.includes('review-launcher'));
  assert.ok(html.includes('sandbox="allow-scripts"'));
  const framePath = html.match(/src="(\/frame\/[^\"]+)"/)[1];
  const frame = await request(framePath);
  assert.equal(frame.headers.get('access-control-allow-origin'), 'null');
  assert.equal(frame.headers.get('access-control-allow-credentials'), null);
  assert.ok((await frame.text()).includes('Image acceptance'));
 }
 assert.equal((await fetch(origin + '/api/shares')).status, 401);
 console.log('PASS: immutable image boots, synthetic fixture login, static/Markdown publication, exact retry receipts, document-first review shell, scoped preview and denied anonymous management.');
})().catch(error => { console.error(error.message); process.exitCode = 1; });
