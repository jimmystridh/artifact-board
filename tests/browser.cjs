'use strict';
/** Intended integration coverage; UNRUN in the current executor (Chromium sockets blocked).
 * Run only against an explicitly started --dev-fixtures loopback instance:
 * ARTIFACT_TEST_ORIGIN=http://127.0.0.1:4317 node tests/browser.cjs
 * Prerequisite: .local/react-built contains the independently built React counter fixture.
 * Never disable Chromium sandboxing to make this suite run.
 */
const { chromium } = require('/opt/codex/runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ORIGIN = process.env.ARTIFACT_TEST_ORIGIN || 'http://127.0.0.1:4317';
assert.ok(['127.0.0.1', 'localhost', '[::1]'].includes(new URL(ORIGIN).hostname), 'Loopback only');
const ROOT = path.resolve(__dirname, '..');
const QA = path.join(ROOT, '.local/qa');
const headers = { 'X-Artifact-Request': '1', Origin: ORIGIN };
const title = `Browser review ${Date.now()}`;
const fixture = `<!doctype html><html><head><style>body{background:#fffdf8;color:#282c25;font:18px Georgia;padding:60px;max-width:740px}h1{font-size:48px;font-weight:400}p{line-height:1.7}button{padding:15px;background:#344b3c;color:white;border:0;border-radius:7px}</style></head><body><span>FIELDNOTES / PRODUCT VISION</span><h1>A calmer way to plan your day.</h1><p id="intro">Make space for the work that matters.</p><button id="cta" onclick="this.textContent='Thanks for your interest'">Explore the idea →</button><h2>Start with less.</h2><p>Three priorities. One clear view.</p><script>try{document.body.dataset.cookie=document.cookie||'empty'}catch(e){document.body.dataset.cookie='blocked'}try{document.body.dataset.parent=parent.document.cookie||'read'}catch(e){document.body.dataset.parent='blocked'}fetch('/api/shares',{credentials:'include'}).then(()=>document.body.dataset.api='unexpected').catch(()=>document.body.dataset.api='blocked')</script></body></html>`;
async function json(context, endpoint, method = 'GET', data) {
 const response = await context.request.fetch(ORIGIN + endpoint, { method, headers, ...(data === undefined ? {} : { data }) });
 assert.ok(response.ok(), `${method} ${endpoint}: ${response.status()} ${await response.text()}`);
 return response.json();
}
async function publish(context, name, files, policy = 'link-only') {
 const share = await json(context, '/api/shares', 'POST', { title: name, kind: 'static', policy, idempotency_key: require('node:crypto').randomUUID() });
 const upload = await json(context, `/api/shares/${share.id}/uploads`, 'POST', {});
 await json(context, upload.upload_url, 'PUT', { files });
 const result = await json(context, `/api/uploads/${upload.id}/finalize`, 'POST', { entrypoint: 'index.html', idempotency_key: crypto.randomUUID(), mutation_actor: 'auth:dev-owner' });
 assert.equal(result.state, 'ready');
 return { share, result, url: result.viewer_url };
}
function allFiles(directory, relative = '') {
 return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => entry.isDirectory()
  ? allFiles(path.join(directory, entry.name), path.posix.join(relative, entry.name))
  : [{ path: path.posix.join(relative, entry.name), content_base64: fs.readFileSync(path.join(directory, entry.name)).toString('base64') }]);
}
(async () => {
 fs.mkdirSync(QA, { recursive: true });
 const browser = await chromium.launch({ executablePath: '/usr/bin/chromium', headless: true, chromiumSandbox: true });
 try {
  const owner = await browser.newContext({ viewport: { width: 1440, height: 1050 } });
  const page = await owner.newPage();
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(ORIGIN); await page.getByRole('button', { name: 'Continue as local publisher' }).click();
  await page.waitForURL(ORIGIN + '/');
  await page.screenshot({ path: path.join(QA, 'dashboard-desktop.png'), fullPage: true });

  // Keyboard, dialog cancellation, explicit policy, and repeated opens.
  await page.locator('#new-share').focus(); await page.keyboard.press('Enter');
  assert.equal(await page.locator('#artifact-policy').inputValue(), '');
  await page.keyboard.press('Escape'); assert.equal(await page.locator('#upload-dialog').isVisible(), false);
  await page.locator('#new-share').click(); await page.locator('#close-upload').click(); await page.locator('#new-share').click();
  assert.equal(await page.locator('#publish-button').isEnabled(), true);
  await page.locator('#artifact-title').fill(title); await page.locator('#artifact-kind').selectOption('static');
  await page.locator('#artifact-policy').selectOption('link-only');
  await page.locator('#artifact-files').setInputFiles({ name: 'index.html', mimeType: 'text/html', buffer: Buffer.from(fixture) });

  // Interrupted upload retry uses its existing draft. Closing/reopening a busy
  // dialog must neither reset its inputs nor send another create/finalize call.
  let creates = 0, finalizes = 0;
  page.on('request', request => { if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/shares') creates++; if (request.method() === 'POST' && request.url().endsWith('/finalize')) finalizes++; });
  await page.route('**/api/uploads/*?token=*', route => route.abort('failed'), { times: 1 });
  await page.locator('#publish-button').click(); await page.waitForFunction(() => document.querySelector('#upload-error').textContent.length > 0);
  assert.equal(creates, 1);
  let releaseFinalize; const holdFinalize = new Promise(resolve => { releaseFinalize = resolve; });
  let buildPolls = 0;
  await page.route('**/api/build-jobs/ui-fixture-job', async route => {
   buildPolls++;
   assert.equal(new URL(page.url()).pathname, '/', 'Queued/running must not navigate to a success view');
   await route.fulfill({ json: { state: buildPolls === 1 ? 'queued' : buildPolls === 2 ? 'running' : 'succeeded' } });
  });
  await page.route('**/api/uploads/*/finalize', async route => {
   await holdFinalize;
   const response = await route.fetch(); assert.ok(response.ok());
   await route.fulfill({ json: { state: 'queued', job_id: 'ui-fixture-job', status_url: '/api/build-jobs/ui-fixture-job' } });
  }, { times: 1 });
  await page.locator('#publish-button').click(); await page.waitForFunction(() => document.querySelector('#upload-progress').textContent.includes('Preparing'));
  await page.locator('#close-upload').click(); await page.locator('#new-share').click();
  assert.equal(await page.locator('#artifact-title').inputValue(), title);
  assert.equal(await page.locator('#publish-button').isDisabled(), true);
  assert.equal(creates, 1); releaseFinalize();
  await page.waitForSelector('.share-card'); await page.getByRole('link', { name: title, exact: true }).click();
  assert.equal(finalizes, 1); assert.equal(buildPolls, 3);
  assert.equal(await page.evaluate(() => sessionStorage.getItem('artifact-pending-build')), null);
  const reviewURL = page.url();
  const frame = page.frameLocator('#artifact-frame');
  await frame.locator('#cta').click(); assert.equal(await frame.locator('#cta').innerText(), 'Thanks for your interest');
  assert.equal(await page.locator('#artifact-frame').getAttribute('sandbox'), 'allow-scripts');

  // The artifact cannot obtain application cookies or call owner APIs.
  await frame.locator('body[data-api]').waitFor();
  assert.equal(await frame.locator('body').getAttribute('data-parent'), 'blocked');
  assert.ok(['blocked', 'empty'].includes(await frame.locator('body').getAttribute('data-cookie')));
  assert.equal(await frame.locator('body').getAttribute('data-api'), 'blocked');

  // Escape cancels explicit element mode without intercepting later clicks.
  await page.locator('#select-element').click(); await frame.locator('h1').focus();
  await page.keyboard.press('Escape'); await page.waitForFunction(() => document.querySelector('#select-element').getAttribute('aria-pressed') === 'false');
  await page.locator('#select-element').click(); await frame.locator('h1').click();
  await page.waitForFunction(() => document.querySelector('#composer-label').textContent === 'Element feedback');
  await page.locator('#comment-body').fill('Can the headline be a little shorter?'); await page.getByRole('button', { name: 'Post comment ↑' }).click();
  await page.waitForSelector('.thread-card');

  // Real DOM range selection creates an independent text anchor.
  await frame.locator('#intro').evaluate(element => { const range = document.createRange(); range.selectNodeContents(element); const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range); element.dispatchEvent(new MouseEvent('mouseup', { bubbles: true })); });
  await page.locator('#comment-selection').click(); await page.locator('#comment-body').fill('This sentence carries the main idea.');
  await page.getByRole('button', { name: 'Post comment ↑' }).click(); await page.waitForFunction(() => document.querySelectorAll('.thread-card').length === 2);
  await page.locator('.thread-card').first().getByRole('button', { name: 'Reply', exact: true }).click();
  await page.locator('.reply-form textarea').fill('Keeping the original context.'); await page.getByRole('button', { name: 'Post reply', exact: true }).click(); await page.waitForSelector('.reply');
  await page.locator('.thread-card').first().getByRole('button', { name: 'Resolve', exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll('.thread-card').length === 1);
  await page.getByRole('button', { name: 'Resolved', exact: true }).click(); assert.equal(await page.locator('.thread-card').count(), 1);
  await page.screenshot({ path: path.join(QA, 'review-desktop.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 }); await page.getByRole('button', { name: 'Open', exact: true }).click();
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'No horizontal overflow');
  await page.screenshot({ path: path.join(QA, 'review-mobile.png'), fullPage: true });
  await page.locator('#toggle-discussion').click(); assert.equal(await page.locator('#discussion-panel').isVisible(), false);
  await page.locator('#toggle-discussion').click(); assert.equal(await page.locator('#discussion-panel').isVisible(), true);

  // Guest identity is explicitly unverified, and guest links grant no management.
  const guest = await browser.newContext(); const guestPage = await guest.newPage(); await guestPage.goto(reviewURL);
  await guestPage.locator('#guest-name').fill('banana'); await guestPage.locator('#comment-body').fill('<img src=x onerror=alert(1)> is just text');
  await guestPage.getByRole('button', { name: 'Post comment ↑' }).click(); await guestPage.getByText('banana', { exact: true }).waitFor();
  assert.ok(await guestPage.locator('.guest-tag').count()); assert.equal(await guestPage.locator('.thread-body img').count(), 0);
  const unauthorized = await guest.request.get(ORIGIN + '/api/shares'); assert.equal(unauthorized.status(), 401);

  // Real built React ES module counter, including gated/null-origin asset CORS.
  const built = allFiles(path.join(ROOT, '.local/react-built')); assert.ok(built.some(file => file.path.endsWith('.js')));
  const react = await publish(owner, 'Built React browser fixture', built, 'entra-gated');
  assert.ok([302,303,307,401].includes((await guest.request.get(ORIGIN + react.url, { maxRedirects: 0 })).status()));
  await json(guest, '/dev/login', 'POST', { identity: 'reviewer' });
  await guestPage.goto(ORIGIN + react.url); const reactFrame = guestPage.frameLocator('#artifact-frame');
  await reactFrame.getByRole('button', { name: 'Count: 0', exact: true }).click(); await reactFrame.getByRole('button', { name: 'Count: 1', exact: true }).waitFor();
  const src = await guestPage.locator('#artifact-frame').getAttribute('src'); assert.match(src, /^\/frame\//);
  const opaque = await browser.newContext();
  const frameResponse = await opaque.request.get(ORIGIN + src, { headers: { Origin: 'null' } }); assert.equal(frameResponse.status(), 200);
  assert.equal(frameResponse.headers()['access-control-allow-origin'], 'null'); assert.equal(frameResponse.headers()['access-control-allow-credentials'], undefined);
  const modulePath = built.find(file => file.path.endsWith('.js')).path;
  const moduleURL = new URL(modulePath, ORIGIN + src).href;
  const moduleResponse = await opaque.request.get(moduleURL, { headers: { Origin: 'null' } }); assert.equal(moduleResponse.status(), 200); assert.equal(moduleResponse.headers()['access-control-allow-origin'], 'null');
  const parts = src.split('/'); const ticket = parts[4];
  assert.equal((await opaque.request.get(ORIGIN + `/api/shares?ticket=${encodeURIComponent(ticket)}`)).status(), 401);
  const altered = [...parts]; altered[3] = 'wrong-revision'; assert.ok((await opaque.request.get(ORIGIN + altered.join('/'))).status() >= 400);
  assert.equal((await opaque.request.get(ORIGIN + `/content/${parts[2]}/${parts[3]}/index.html`)).status(), 401);
  await guestPage.screenshot({ path: path.join(QA, 'react-gated-counter.png'), fullPage: true });
  assert.deepEqual(errors, [], 'No shell JavaScript errors');
  console.log('PASS: desktop/mobile, keyboard, interrupted upload/retry, selection, discussion, guests, sandbox boundaries, gated React/module CORS.');
 } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
