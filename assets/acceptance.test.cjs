'use strict';
// Deterministic shell acceptance regressions. These exercise app.js directly in a
// small DOM harness; they do not replace the separately blocked browser suite.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { webcrypto } = require('node:crypto');
const source = fs.readFileSync(require('node:path').join(__dirname, 'app.js'), 'utf8');

class Element {
 constructor(tag = 'div') { this.tagName = tag.toLowerCase(); this.children = []; this.parentElement = null; this.dataset = {}; this.style = {}; this.attributes = {}; this.value = ''; this.className = ''; this._text = ''; this.disabled = false; }
 get textContent() { return this._text + this.children.map(child => child.textContent).join(''); }
 set textContent(value) { this._text = String(value); this.children = []; }
 get classList() {
  const el = this;
  return { add: (...names) => { el.className = [...new Set([...el.className.split(/\s+/).filter(Boolean), ...names])].join(' '); }, remove: (...names) => { el.className = el.className.split(/\s+/).filter(name => !names.includes(name)).join(' '); }, toggle(name, force) { const present = el.className.split(/\s+/).includes(name), add = force === undefined ? !present : force; this[add ? 'add' : 'remove'](name); return add; } };
 }
 append(...children) { children.forEach(child => { child.parentElement = this; this.children.push(child); }); }
 replaceChildren(...children) { this.children.forEach(child => { child.parentElement = null; }); this.children = []; this._text = ''; this.append(...children); }
 setAttribute(name, value) { this.attributes[name] = String(value); }
 getAttribute(name) { return this.attributes[name]; }
 addEventListener() {}
 focus() { this.focused = true; }
 scrollIntoView() {}
 showModal() { this.open = true; }
 close() { this.open = false; }
 reset() {}
 matches(selector) {
  if (selector.startsWith('#')) return this.id === selector.slice(1);
  if (selector.startsWith('.')) return this.className.split(/\s+/).includes(selector.slice(1));
  if (selector.startsWith('[data-')) { const key = selector.slice(6, -1).replace(/-([a-z])/g, (_, char) => char.toUpperCase()); return this.dataset[key] !== undefined; }
  const match = selector.match(/^([a-z]+)(?:\[([^=]+)=([^\]]+)\])?$/);
  return !!match && this.tagName === match[1] && (!match[2] || this[match[2]] === match[3]);
 }
 querySelectorAll(selector) {
  const parts = selector.split(' '), all = [];
  const walk = node => node.children.forEach(child => { all.push(child); walk(child); }); walk(this);
  return all.filter(node => { if (!node.matches(parts.at(-1))) return false; let parent = node.parentElement; for (let index = parts.length - 2; index >= 0; index--) { while (parent && !parent.matches(parts[index])) parent = parent.parentElement; if (!parent) return false; parent = parent.parentElement; } return true; });
 }
 querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
}
const thread = (id, body = 'Original comment') => ({ id, share_id: 'share-1', original_revision: 'revision-1', original_anchor: { kind: 'general' }, resolved: false, comments: [{ id: `${id}-comment`, body, display_name: 'Reviewer', authenticated: false }] });
const response = data => ({ ok: true, status: 200, json: async () => structuredClone(data) });
function mutationResponse(call, data) {
 const value = structuredClone(data); value.comments = value.comments.map((comment, index) => ({ ...comment, id: comment.id || `${value.id}-reply-${index}` }));
 const comment = value.comments.findLast(comment => comment.body === call.body.body); assert.ok(comment, 'A real receipt must contain the submitted comment');
 value._mutation = { idempotency_key: call.body.idempotency_key, operation: call.url.endsWith('/replies') ? 'reply' : 'thread', comment_id: comment.id };
 return response(value);
}
const published = () => ({ state: 'ready', share_id: 'share-new', revision_id: 'revision-new', viewer_url: '/s/new-review-token' });
const queued = () => ({ state: 'queued', share_id: 'share-new', job_id: 'job-original', status_url: '/api/build-jobs/job-original' });
const failure = message => ({ ok: false, status: 503, json: async () => ({ message }) });
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
const storage = () => { const values = new Map(); return { values, getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, String(value)), removeItem: key => values.delete(key) }; };
async function harness(initialThreads, fetcher, config = {}) {
 const body = new Element('body'), app = new Element(); app.id = 'app'; app.dataset.page = config.page || 'review'; app.dataset.boot = JSON.stringify({ capability: 'review-token', mutation_actor: 'guest:actor-1', revision: { id: 'revision-1' }, share: { id: 'share-1' }, can_manage: true, threads: initialThreads, ...config.boot }); body.append(app);
 const ids = ['guest-name', 'artifact-frame', 'anchor-preview', 'composer-label', 'selection-action', 'discussion-panel', 'comment-body', 'select-element', 'selection-hint', 'comment-selection', 'cancel-anchor', 'general-comment', 'new-comment', 'toggle-discussion', 'copy-review-link', 'thread-list', 'thread-count', 'refresh-discussion', 'comment-form', 'comment-error', 'toast', 'sign-out', 'upload-dialog', 'upload-form', 'publish-button', 'upload-title', 'artifact-title', 'artifact-kind', 'artifact-policy', 'entrypoint', 'artifact-files', 'file-summary', 'upload-progress', 'upload-error', 'new-share', 'close-upload', 'policy-field', 'policy-summary', 'share-search', 'no-results'];
 ids.forEach(id => { const node = new Element(id === 'comment-form' ? 'form' : id === 'comment-body' ? 'textarea' : 'div'); node.id = id; app.append(node); });
 const identity = new Element('p'); identity.className = 'identity-note'; app.append(identity);
 const filters = {};
 for (const name of ['open', 'resolved', 'unmatched']) { const button = new Element('button'); button.dataset.threadFilter = name; app.append(button); filters[name] = button; }
 const $ = selector => body.querySelector(selector);
 $('#guest-name').value = 'Guest'; $('#artifact-frame').contentWindow = { postMessage() {} };
 const submit = new Element('button'); submit.type = 'submit'; $('#comment-form').append(submit);
 const document = { querySelector: $, querySelectorAll: selector => body.querySelectorAll(selector), getElementById: id => $(`#${id}`), createElement: tag => new Element(tag), addEventListener() {} };
 const calls = [], sessionStorage = config.storage || storage(); let reloads = 0;
 $('#artifact-files').files = [];
 const context = { document, sessionStorage, TextEncoder, URL, AbortSignal, window: { addEventListener() {} }, crypto: webcrypto, fetch: async (url, options = {}) => { const call = { url: String(url), method: options.method || 'GET', body: typeof options.body === 'string' ? JSON.parse(options.body) : options.body }; calls.push(call); return fetcher(call); }, location: { origin: 'http://localhost', href: 'http://localhost/s/review-token', reload() { reloads++; } }, navigator: {}, setTimeout: () => 1, clearTimeout() {}, matchMedia: () => ({ matches: false }), console };
 await vm.runInNewContext(source, context, { timeout: 1000 });
 const card = id => $('#thread-list').querySelectorAll('.thread-card').find(node => node.dataset.id === id);
 const action = (id, label) => card(id).querySelectorAll('button').find(node => node.textContent === label);
 const openReply = id => { action(id, 'Reply').onclick(); return card(id).querySelector('.reply-form'); };
 const submitReply = id => card(id).querySelector('.reply-form').onsubmit({ preventDefault() {} });
 return { $, filters, calls, card, action, openReply, submitReply, storage: sessionStorage, get reloads() { return reloads; }, submitComment: () => $('#comment-form').onsubmit({ preventDefault() {}, target: $('#comment-form') }), submitUpload: () => $('#upload-form').onsubmit({ preventDefault() {} }) };
}

test('a saved reply is reconciled from POST without a fallible follow-up GET or duplicate retry', async () => {
 const original = thread('one'), saved = { ...original, comments: [...original.comments, { id: 'reply-1', body: 'Saved reply', display_name: 'Guest' }] };
 const h = await harness([original], call => call.method === 'POST' ? mutationResponse(call, saved) : failure('Refresh unavailable'));
 h.openReply('one').querySelector('textarea').value = 'Saved reply';
 await h.submitReply('one');
 assert.equal(h.calls.length, 1); assert.equal(h.calls[0].method, 'POST');
 assert.match(h.card('one').textContent, /Saved reply/); assert.equal(h.card('one').querySelector('.reply-form'), null);
 assert.equal(h.$('#toast').textContent, 'Reply posted');
 h.openReply('one'); assert.equal(h.card('one').querySelector('textarea').value, '');
});

test('unsent reply drafts survive discussion refresh and filter changes', async () => {
 const original = thread('one'), h = await harness([original], () => response([original]));
 h.openReply('one').querySelector('textarea').value = 'Keep this draft';
 await h.$('#refresh-discussion').onclick();
 assert.equal(h.card('one').querySelector('textarea').value, 'Keep this draft');
 h.filters.resolved.onclick(); assert.equal(h.card('one'), undefined);
 h.filters.open.onclick(); assert.equal(h.card('one').querySelector('textarea').value, 'Keep this draft');
});

test('failed reply keeps its draft and inline error across filters, then retries once', async () => {
 const original = thread('one'); let attempts = 0;
 const h = await harness([original], call => ++attempts === 1 ? failure('Try again later') : mutationResponse(call, { ...original, comments: [...original.comments, { body: 'Retry me' }] }));
 h.openReply('one').querySelector('textarea').value = 'Retry me'; await h.submitReply('one');
 let form = h.card('one').querySelector('.reply-form'); assert.equal(form.querySelector('textarea').value, 'Retry me'); assert.equal(form.querySelector('button').disabled, false); assert.match(form.textContent, /Try again later/);
 h.filters.resolved.onclick(); h.filters.open.onclick(); form = h.card('one').querySelector('.reply-form'); assert.match(form.textContent, /Try again later/);
 await h.submitReply('one'); assert.equal(attempts, 2); assert.equal(h.card('one').querySelector('.reply-form'), null);
});

test('pending reply stays disabled across rerenders and repeated submission cannot duplicate it', async () => {
 const original = thread('one'), pending = deferred(), h = await harness([original], () => pending.promise);
 h.openReply('one').querySelector('textarea').value = 'Only once'; const posting = h.submitReply('one');
 h.filters.resolved.onclick(); h.filters.open.onclick();
 const form = h.card('one').querySelector('.reply-form'); assert.equal(form.querySelector('button').disabled, true); assert.equal(form.querySelector('textarea').disabled, true);
 await h.submitReply('one'); assert.equal(h.calls.length, 1);
 pending.resolve(mutationResponse(h.calls[0], { ...original, comments: [...original.comments, { body: 'Only once' }] })); await posting;
 assert.equal(h.card('one').querySelector('.reply-form'), null);
});

test('posting one reply preserves a different thread’s draft', async () => {
 const one = thread('one'), two = thread('two'), h = await harness([one, two], call => mutationResponse(call, { ...one, comments: [...one.comments, { body: 'Posted one' }] }));
 h.openReply('one').querySelector('textarea').value = 'Posted one'; h.openReply('two').querySelector('textarea').value = 'Unsent two';
 await h.submitReply('one'); assert.equal(h.card('two').querySelector('textarea').value, 'Unsent two');
});

test('an older in-flight refresh cannot remove a newly acknowledged reply', async () => {
 const original = thread('one'), stale = deferred(), h = await harness([original], call => call.method === 'GET' ? stale.promise : mutationResponse(call, { ...original, comments: [...original.comments, { body: 'Newest reply' }] }));
 const refreshing = h.$('#refresh-discussion').onclick(); h.openReply('one').querySelector('textarea').value = 'Newest reply'; await h.submitReply('one');
 stale.resolve(response([original])); await refreshing; assert.match(h.card('one').textContent, /Newest reply/);
});

test('resolving a thread does not discard its unfinished reply', async () => {
 const original = thread('one'), resolved = { ...original, resolved: true }, h = await harness([original], call => response(call.method === 'POST' ? resolved : [resolved]));
 h.openReply('one').querySelector('textarea').value = 'Return to this'; await h.action('one', 'Resolve').onclick();
 assert.equal(h.card('one'), undefined); h.filters.resolved.onclick(); assert.equal(h.card('one').querySelector('textarea').value, 'Return to this');
});

const recoveryStates = store => [...store.values.values()].map(value => JSON.parse(value).state);
const write = (element, value) => { element.value = value; if (element.oninput) element.oninput(); };

test('a lost reply acknowledgement retries exactly the same persisted request after reload', async () => {
 const original = thread('one'), store = storage(), receipt = { ...original, comments: [...original.comments, { id: 'saved-reply', body: 'Stored once' }] };
 const first = await harness([original], () => { throw Error('Response lost'); }, { storage: store });
 write(first.openReply('one').querySelector('textarea'), 'Stored once'); await first.submitReply('one');
 const sent = first.calls[0].body;
 assert.ok(sent.idempotency_key); assert.equal(sent.mutation_actor, 'guest:actor-1');
 assert.deepEqual(recoveryStates(store)[0].replies.one.request, sent);
 const second = await harness([receipt], call => mutationResponse(call, receipt), { storage: store });
 assert.match(second.card('one').textContent, /Retry original reply/);
 await second.submitReply('one');
 assert.deepEqual(second.calls[0].body, sent); assert.equal(second.card('one').querySelectorAll('.thread-body').filter(item => item.textContent === 'Stored once').length, 1);
 assert.equal(recoveryStates(store)[0].replies.one, undefined);
});

test('edited reply stays a separate draft until the original pending request is confirmed', async () => {
 const original = thread('one'), store = storage(); let attempts = 0;
 const receipt = { ...original, comments: [...original.comments, { id: 'saved', body: 'Original request' }] };
 const first = await harness([original], () => { throw Error('Response lost'); }, { storage: store });
 write(first.openReply('one').querySelector('textarea'), 'Original request'); await first.submitReply('one');
 write(first.card('one').querySelector('textarea'), 'Edited draft');
 const second = await harness([original], call => mutationResponse(call, attempts++ === 0 ? receipt : { ...receipt, comments: [...receipt.comments, { id: 'edited', body: call.body.body }] }), { storage: store });
 assert.equal(second.card('one').querySelector('textarea').value, 'Edited draft');
 await second.submitReply('one');
 assert.deepEqual(second.calls[0].body, first.calls[0].body); assert.equal(second.card('one').querySelector('textarea').value, 'Edited draft');
 assert.equal(recoveryStates(store)[0].replies.one.request, null);
 await second.submitReply('one');
 assert.equal(second.calls[1].body.body, 'Edited draft'); assert.notEqual(second.calls[1].body.idempotency_key, second.calls[0].body.idempotency_key);
});

test('a lost new-comment acknowledgement survives reload and reconciles without another GET', async () => {
 const store = storage(), receipt = thread('created', 'A new comment');
 const first = await harness([], () => { throw Error('Response lost'); }, { storage: store });
 write(first.$('#comment-body'), 'A new comment'); await first.submitComment();
 assert.deepEqual(recoveryStates(store)[0].composer.request, first.calls[0].body);
 const second = await harness([], call => mutationResponse(call, receipt), { storage: store });
 assert.equal(second.$('#comment-body').value, 'A new comment'); assert.match(second.$('#comment-form').textContent, /Retry original comment/);
 await second.submitComment();
 assert.equal(second.calls.length, 1); assert.deepEqual(second.calls[0].body, first.calls[0].body);
 assert.equal(second.$('#comment-body').value, ''); assert.match(second.card('created').textContent, /A new comment/);
 assert.equal(recoveryStates(store)[0].composer.request, null);
});

test('changed comment and guest name remain a draft while retrying the original immutable payload', async () => {
 const store = storage(), receipt = thread('created', 'First wording');
 const first = await harness([], () => { throw Error('Response lost'); }, { storage: store });
 write(first.$('#comment-body'), 'First wording'); await first.submitComment();
 write(first.$('#comment-body'), 'Revised wording'); write(first.$('#guest-name'), 'Another display name');
 const second = await harness([], call => mutationResponse(call, thread('created', call.body.body)), { storage: store });
 await second.submitComment();
 assert.deepEqual(second.calls[0].body, first.calls[0].body); assert.equal(second.$('#comment-body').value, 'Revised wording');
 assert.equal(second.$('#guest-name').value, 'Another display name'); assert.equal(recoveryStates(store)[0].composer.request, null);
 await second.submitComment();
 assert.equal(second.calls[1].body.body, 'Revised wording'); assert.equal(second.calls[1].body.guest_name, 'Another display name'); assert.notEqual(second.calls[1].body.idempotency_key, second.calls[0].body.idempotency_key);
});

test('repeated new-comment submits while pending issue only one request', async () => {
 const pending = deferred(), h = await harness([], () => pending.promise);
 write(h.$('#comment-body'), 'Only once'); const posting = h.submitComment();
 await h.submitComment(); assert.equal(h.calls.length, 1); assert.equal(h.$('#comment-body').disabled, true);
 pending.resolve(mutationResponse(h.calls[0], thread('one', 'Only once'))); await posting;
 assert.equal(h.$('#comment-body').disabled, false); assert.equal(h.$('#comment-body').value, '');
});

test('replaying an older receipt preserves newer comments already in the discussion', async () => {
 const original = thread('one'), store = storage();
 const first = await harness([original], () => { throw Error('Response lost'); }, { storage: store });
 write(first.openReply('one').querySelector('textarea'), 'Recovered reply'); await first.submitReply('one');
 const receipt = { ...original, comments: [...original.comments, { id: 'recovered', body: 'Recovered reply' }] };
 const latest = { ...receipt, comments: [...receipt.comments, { id: 'newer', body: 'A newer reply' }] };
 const second = await harness([latest], call => mutationResponse(call, receipt), { storage: store });
 await second.submitReply('one');
 assert.match(second.card('one').textContent, /A newer reply/); assert.match(second.card('one').textContent, /Recovered reply/);
 assert.equal(second.card('one').querySelectorAll('.thread-body').length, 3);
});

test('review recovery is isolated by actor, share, and capability without storing the capability', async () => {
 const store = storage(), first = await harness([], () => { throw Error('Response lost'); }, { storage: store });
 write(first.$('#comment-body'), 'Private pending draft'); await first.submitComment();
 const serialized = [...store.values.entries()].flat().join(''); assert.doesNotMatch(serialized, /review-token/);
 for (const boot of [{ mutation_actor: 'guest:actor-2' }, { share: { id: 'share-2' } }, { capability: 'another-review-token' }]) {
  const other = await harness([], () => { throw Error('Should not write'); }, { storage: store, boot });
  assert.equal(other.$('#comment-body').value, ''); assert.equal(other.calls.length, 0);
 }
});

test('missing guest identity is initialized before a new request and is never regenerated for a pending retry', async () => {
 const store = storage(), first = await harness([], call => call.url.endsWith('/identity') ? response({ mutation_actor: 'guest:preflight' }) : { ok: false, status: 409, json: async () => ({ error: { code: 'actor_changed', message: 'Identity changed. Reload.' } }) }, { storage: store, boot: { mutation_actor: undefined } });
 write(first.$('#comment-body'), 'Bound to initial actor'); await first.submitComment(); await first.submitComment();
 assert.equal(first.calls.filter(call => call.url.endsWith('/identity')).length, 1);
 assert.equal(first.calls[1].body.mutation_actor, 'guest:preflight'); assert.deepEqual(first.calls[2].body, first.calls[1].body);
});

test('unreadable success responses preserve the exact pending request', async () => {
 const h = await harness([], () => ({ ok: true, status: 200, json: async () => { throw Error('Truncated JSON'); } }));
 write(h.$('#comment-body'), 'Keep request'); await h.submitComment(); await h.submitComment();
 assert.deepEqual(h.calls[0].body, h.calls[1].body); assert.match(h.$('#comment-error').textContent, /could not be read/);
});

test('unavailable durable storage blocks writes and retains the visible draft', async () => {
 const store = storage(); store.setItem = () => { throw Error('Storage unavailable'); };
 const h = await harness([], () => { throw Error('Should not post'); }, { storage: store });
 write(h.$('#comment-body'), 'Visible draft'); await h.submitComment();
 assert.equal(h.calls.length, 0); assert.equal(h.$('#comment-body').value, 'Visible draft'); assert.match(h.$('#comment-error').textContent, /session storage/);
});

test('failed acknowledgement persistence keeps the same key safe for retry', async () => {
 const store = storage(), save = store.setItem; let rejectAcknowledgement = false;
 store.setItem = (key, value) => { if (rejectAcknowledgement && !JSON.parse(value).state.composer.request) throw Error('Storage full'); save(key, value); };
 const h = await harness([], call => { rejectAcknowledgement = true; return mutationResponse(call, thread('saved', 'Durable reply')); }, { storage: store });
 write(h.$('#comment-body'), 'Durable reply'); await h.submitComment(); await h.submitComment();
 assert.deepEqual(h.calls[0].body, h.calls[1].body); assert.equal(h.$('#comment-body').value, 'Durable reply');
});

async function dashboard(fetcher, config = {}) { return harness([], fetcher, { ...config, page: 'dashboard', boot: { mutation_actor: 'auth:publisher-1', principal: { id: 'publisher-1' }, ...config.boot } }); }
function chooseUpload(h) { h.$('#new-share').onclick(); h.$('#artifact-title').value = 'An artifact'; h.$('#artifact-kind').value = 'static'; h.$('#artifact-policy').value = 'link-only'; h.$('#entrypoint').value = 'index.html'; h.$('#artifact-files').files = [{ name: 'site.zip', size: 12 }]; }
function uploadFetcher(finalize) { return call => { if (call.url === '/api/shares') return response({ id: 'share-new' }); if (call.url.endsWith('/uploads')) return response({ id: 'upload-1', upload_url: '/api/uploads/upload-1/content' }); if (call.method === 'PUT') return response({ ok: true }); if (call.url.endsWith('/finalize')) return finalize(call); throw Error(`Unexpected request: ${call.url}`); }; }

test('lost finalize acknowledgement retries the same upload, entrypoint and key after reload', async () => {
 const store = storage(), first = await dashboard(uploadFetcher(() => { throw Error('Finalization response lost'); }), { storage: store });
 chooseUpload(first); await first.submitUpload(); const sent = first.calls.find(call => call.url.endsWith('/finalize'));
 assert.equal(sent.body.mutation_actor, 'auth:publisher-1'); assert.equal(recoveryStates(store)[0].finalize.upload_id, 'upload-1');
 const second = await dashboard(uploadFetcher(() => response(published())), { storage: store });
 assert.equal(second.$('#upload-dialog').open, true); assert.equal(second.$('#artifact-files').files.length, 0); assert.equal(second.$('#upload-form').noValidate, true);
 second.$('#entrypoint').value = 'changed.html'; await second.submitUpload();
 assert.equal(second.calls.length, 1); assert.deepEqual(second.calls[0], sent); assert.equal(second.reloads, 1);
 assert.equal(recoveryStates(store)[0].finalize, null);
});

test('repeated finalize submissions while pending and reopen do not create another upload', async () => {
 const pending = deferred(), h = await dashboard(uploadFetcher(() => pending.promise));
 chooseUpload(h); const posting = h.submitUpload();
 while (!h.calls.some(call => call.url.endsWith('/finalize'))) await Promise.resolve();
 await h.submitUpload(); h.$('#close-upload').onclick(); h.$('#new-share').onclick();
 assert.equal(h.calls.filter(call => call.url.endsWith('/uploads')).length, 1); assert.equal(h.calls.filter(call => call.url.endsWith('/finalize')).length, 1);
 pending.resolve(response(published())); await posting; assert.equal(h.reloads, 1);
});

test('a recovered queued finalize transitions to the same durable build job', async () => {
 const store = storage(), first = await dashboard(uploadFetcher(() => { throw Error('Response lost'); }), { storage: store });
 chooseUpload(first); await first.submitUpload();
 const second = await dashboard(call => call.url.endsWith('/finalize') ? response(queued()) : failure('Status unavailable'), { storage: store });
 await second.submitUpload();
 assert.equal(second.calls[0].url, '/api/uploads/upload-1/finalize'); assert.equal(second.calls[1].url, 'http://localhost/api/build-jobs/job-original');
 assert.equal(recoveryStates(store)[0].finalize, null); assert.equal(recoveryStates(store)[0].build.status_url, '/api/build-jobs/job-original'); assert.equal(second.reloads, 0);
 const third = await dashboard(() => response({ id: 'job-original', share_id: 'share-new', state: 'succeeded', revision_id: 'revision-new', result: { id: 'share-new', state: 'published', revision_id: 'revision-new' } }), { storage: store });
 await new Promise(resolve => setImmediate(resolve));
 assert.equal(third.calls.length, 1); assert.equal(third.calls[0].method, 'GET'); assert.equal(third.reloads, 1); assert.equal(recoveryStates(store)[0].build, null);
});

test('another publisher cannot recover a different actor’s pending finalize', async () => {
 const store = storage(), first = await dashboard(uploadFetcher(() => { throw Error('Response lost'); }), { storage: store });
 chooseUpload(first); await first.submitUpload();
 const second = await dashboard(() => { throw Error('Should not write'); }, { storage: store, boot: { mutation_actor: 'auth:publisher-2', principal: { id: 'publisher-2' } } });
 assert.equal(second.calls.length, 0); assert.notEqual(second.$('#upload-dialog').open, true); second.$('#new-share').onclick();
 assert.equal(second.$('#entrypoint').disabled, false); assert.equal(second.$('#upload-form').noValidate, false);
});

const rejected = (status, code, message) => ({ ok: false, status, json: async () => ({ error: { code, message } }) });
test('acknowledged comment validation rejection preserves the draft but allows a corrected new request', async () => {
 let attempts = 0;
 const h = await harness([], call => attempts++ === 0 ? rejected(400, 'invalid_comment', 'Correct the body.') : mutationResponse(call, thread('saved', call.body.body)));
 write(h.$('#comment-body'), 'Rejected wording'); await h.submitComment();
 assert.equal(recoveryStates(h.storage)[0].composer.request, null); assert.equal(h.$('#comment-body').value, 'Rejected wording');
 assert.match(h.$('#comment-error').textContent, /Correct the body/);
 write(h.$('#comment-body'), 'Corrected wording'); await h.submitComment();
 assert.notEqual(h.calls[1].body.idempotency_key, h.calls[0].body.idempotency_key); assert.equal(h.calls[1].body.body, 'Corrected wording');
});

test('acknowledged reply validation rejection retains an editable draft with a new key on correction', async () => {
 const original = thread('one'); let attempts = 0;
 const h = await harness([original], call => attempts++ === 0 ? rejected(413, 'comment_response_limit', 'Correct the reply.') : mutationResponse(call, { ...original, comments: [...original.comments, { id: 'new', body: call.body.body }] }));
 write(h.openReply('one').querySelector('textarea'), 'Rejected reply'); await h.submitReply('one');
 assert.equal(recoveryStates(h.storage)[0].replies.one.request, null); assert.equal(h.card('one').querySelector('textarea').value, 'Rejected reply');
 write(h.card('one').querySelector('textarea'), 'Corrected reply'); await h.submitReply('one');
 assert.notEqual(h.calls[1].body.idempotency_key, h.calls[0].body.idempotency_key); assert.equal(h.calls[1].body.body, 'Corrected reply');
});

test('coded finalize preparation rejection unlocks retained files and a corrected new upload', async () => {
 let attempts = 0;
 const h = await dashboard(uploadFetcher(() => attempts++ === 0 ? rejected(422, 'preparation_failed', 'Entrypoint does not exist.') : response(published())));
 chooseUpload(h); await h.submitUpload();
 assert.equal(recoveryStates(h.storage)[0].finalize, null); assert.equal(h.$('#artifact-files').files.length, 1); assert.equal(h.$('#entrypoint').disabled, false); assert.equal(h.$('#upload-form').noValidate, false);
 h.$('#entrypoint').value = 'corrected.html'; await h.submitUpload();
 const finals = h.calls.filter(call => call.url.endsWith('/finalize'));
 assert.equal(h.calls.filter(call => call.url.endsWith('/uploads')).length, 2); assert.equal(finals[1].body.entrypoint, 'corrected.html'); assert.notEqual(finals[1].body.idempotency_key, finals[0].body.idempotency_key); assert.equal(h.reloads, 1);
});

test('uncoded validation errors, conflicts, and malformed success receipts remain unconfirmed', async () => {
 for (const result of [rejected(400, undefined, 'Bad request'), rejected(409, 'idempotency_conflict', 'Conflicting retry'), response({}), { ok: false, status: 422, json: async () => { throw Error('Incomplete response'); } }]) {
  const h = await harness([], () => result);
  write(h.$('#comment-body'), 'Original'); await h.submitComment(); write(h.$('#comment-body'), 'Edited'); await h.submitComment();
  assert.deepEqual(h.calls[1].body, h.calls[0].body); assert.equal(h.$('#comment-body').value, 'Edited'); assert.ok(recoveryStates(h.storage)[0].composer.request);
 }
});

test('malformed successful reply and finalize responses cannot drop their pending keys', async () => {
 const original = thread('one'), reply = await harness([original], () => response({ id: 'wrong-thread' }));
 write(reply.openReply('one').querySelector('textarea'), 'Pending'); await reply.submitReply('one'); await reply.submitReply('one');
 assert.deepEqual(reply.calls[1].body, reply.calls[0].body);
 const publish = await dashboard(uploadFetcher(() => response({}))); chooseUpload(publish); await publish.submitUpload(); await publish.submitUpload();
 const finals = publish.calls.filter(call => call.url.endsWith('/finalize')); assert.deepEqual(finals[0].body, finals[1].body); assert.equal(publish.calls.filter(call => call.url.endsWith('/uploads')).length, 1);
});

test('unsent comment and reply drafts survive reload without any write request', async () => {
 const original = thread('one'), store = storage(), first = await harness([original], () => { throw Error('Should not post'); }, { storage: store });
 write(first.$('#comment-body'), 'Unsent comment'); write(first.openReply('one').querySelector('textarea'), 'Unsent reply');
 const second = await harness([original], () => { throw Error('Should not post'); }, { storage: store });
 assert.equal(second.$('#comment-body').value, 'Unsent comment'); assert.equal(second.card('one').querySelector('textarea').value, 'Unsent reply'); assert.equal(second.calls.length, 0);
});

test('a definitively failed recovered build unlocks correction on the original share', async () => {
 const store = storage(), first = await dashboard(uploadFetcher(() => { throw Error('Response lost'); }), { storage: store });
 chooseUpload(first); await first.submitUpload();
 const second = await dashboard(call => call.url.endsWith('/finalize') ? response(queued()) : failure('Status unavailable'), { storage: store });
 await second.submitUpload();
 const third = await dashboard(call => call.method === 'GET' ? response({ id: 'job-original', share_id: 'share-new', state: 'failed', error: 'Correct your source project.' }) : uploadFetcher(() => response(published()))(call), { storage: store });
 await new Promise(resolve => setImmediate(resolve));
 assert.equal(third.$('#artifact-files').disabled, false); assert.equal(third.$('#entrypoint').disabled, false); assert.equal(third.$('#artifact-title').value, 'An artifact'); assert.equal(recoveryStates(store)[0].build, null);
 third.$('#artifact-files').files = [{ name: 'corrected.zip', size: 12 }]; third.$('#entrypoint').value = 'corrected.html'; await third.submitUpload();
 assert.equal(third.calls.some(call => call.url === '/api/shares'), false); assert.equal(third.calls.filter(call => call.url.endsWith('/uploads')).length, 1); assert.equal(third.reloads, 1);
});

test('a malformed queued build destination retains finalize recovery instead of acknowledging it', async () => {
 for (const status_url of [undefined, 'https://elsewhere.invalid/api/build-jobs/job-1', '/unexpected/job-1']) {
  const h = await dashboard(uploadFetcher(() => response({ ...queued(), status_url }))); chooseUpload(h); await h.submitUpload(); await h.submitUpload();
  const finals = h.calls.filter(call => call.url.endsWith('/finalize')); assert.deepEqual(finals[0].body, finals[1].body);
  assert.equal(h.calls.some(call => call.method === 'GET'), false); assert.ok(recoveryStates(h.storage)[0].finalize); assert.equal(recoveryStates(h.storage)[0].build, null);
 }
});

test('an incomplete thread receipt with the expected id is still unconfirmed', async () => {
 const original = thread('one'), h = await harness([original], () => response({ id: 'one' }));
 write(h.openReply('one').querySelector('textarea'), 'Pending reply'); await h.submitReply('one'); await h.submitReply('one');
 assert.deepEqual(h.calls[0].body, h.calls[1].body); assert.ok(recoveryStates(h.storage)[0].replies.one.request);
});

test('unknown coded 400, 413, and 422 outcomes keep comment and finalize requests unconfirmed', async () => {
 for (const status of [400, 413, 422]) {
  const h = await harness([], () => rejected(status, 'unknown_failure', 'Unknown outcome.'));
  write(h.$('#comment-body'), 'Original request'); await h.submitComment(); write(h.$('#comment-body'), 'Edited draft'); await h.submitComment();
  assert.deepEqual(h.calls[0].body, h.calls[1].body); assert.ok(recoveryStates(h.storage)[0].composer.request);
  const publish = await dashboard(uploadFetcher(() => rejected(status, 'unknown_failure', 'Unknown outcome.')));
  chooseUpload(publish); await publish.submitUpload(); await publish.submitUpload();
  const finals = publish.calls.filter(call => call.url.endsWith('/finalize')); assert.deepEqual(finals[0].body, finals[1].body);
  assert.equal(publish.calls.filter(call => call.url.endsWith('/uploads')).length, 1); assert.ok(recoveryStates(publish.storage)[0].finalize);
 }
});

test('an unchanged but valid 200 thread snapshot cannot acknowledge a pending reply', async () => {
 const original = thread('one'), h = await harness([original], () => response(original));
 write(h.openReply('one').querySelector('textarea'), 'Not in this snapshot'); await h.submitReply('one'); await h.submitReply('one');
 assert.deepEqual(h.calls[0].body, h.calls[1].body); assert.equal(h.card('one').querySelector('textarea').value, 'Not in this snapshot'); assert.ok(recoveryStates(h.storage)[0].replies.one.request);
});

test('thread and reply acknowledgements require the exact mutation key, operation, comment ID, body and share', async () => {
 const corruptions = [
  data => { delete data._mutation; },
  data => { data._mutation.idempotency_key = 'another-key'; },
  data => { data._mutation.operation = 'another-operation'; },
  data => { data._mutation.comment_id = 'another-comment'; },
  data => { data.comments.find(comment => comment.id === data._mutation.comment_id).body = 'Different submitted body'; },
  data => { data.share_id = 'another-share'; },
 ];
 for (const operation of ['thread', 'reply']) {
  for (const corrupt of corruptions) {
   const original = thread('one'), h = await harness(operation === 'reply' ? [original] : [], async call => {
    const saved = operation === 'reply' ? { ...original, comments: [...original.comments, { id: 'new-comment', body: call.body.body }] } : thread('created', call.body.body);
    const data = await mutationResponse(call, saved).json(); corrupt(data); return response(data);
   });
   if (operation === 'reply') { write(h.openReply('one').querySelector('textarea'), 'Pending mutation'); await h.submitReply('one'); await h.submitReply('one'); assert.ok(recoveryStates(h.storage)[0].replies.one.request); }
   else { write(h.$('#comment-body'), 'Pending mutation'); await h.submitComment(); await h.submitComment(); assert.ok(recoveryStates(h.storage)[0].composer.request); }
   assert.deepEqual(h.calls[0].body, h.calls[1].body);
  }
 }
});

test('a forged matching receipt marker cannot acknowledge the original unrelated comment', async () => {
 const original = thread('one'), h = await harness([original], call => response({ ...original, _mutation: { idempotency_key: call.body.idempotency_key, operation: 'reply', comment_id: original.comments[0].id } }));
 write(h.openReply('one').querySelector('textarea'), 'New reply'); await h.submitReply('one'); await h.submitReply('one');
 assert.deepEqual(h.calls[0].body, h.calls[1].body); assert.ok(recoveryStates(h.storage)[0].replies.one.request);
});

test('ready receipts must identify this share, a revision, and a valid same-origin review URL', async () => {
 for (const receipt of [
  { state: 'ready' },
  { ...published(), share_id: 'another-share' },
  { ...published(), revision_id: '' },
  { ...published(), viewer_url: 'https://elsewhere.invalid/s/token' },
  { ...published(), viewer_url: '/unexpected/token' },
  { ...published(), viewer_url: '/s/token?unverified=1' },
 ]) {
  const h = await dashboard(uploadFetcher(() => response(receipt))); chooseUpload(h); await h.submitUpload(); await h.submitUpload();
  const finals = h.calls.filter(call => call.url.endsWith('/finalize')); assert.deepEqual(finals[0].body, finals[1].body); assert.ok(recoveryStates(h.storage)[0].finalize); assert.equal(h.reloads, 0);
 }
});

test('queued receipts must bind this share and the exact declared job destination', async () => {
 for (const receipt of [
  { ...queued(), share_id: 'another-share' },
  { ...queued(), job_id: undefined },
  { ...queued(), job_id: 'another-job' },
  { ...queued(), status_url: '/api/build-jobs/another-job' },
  { ...queued(), status_url: '/api/build-jobs/job-original/extra' },
  { ...queued(), status_url: '/api/build-jobs/job-original?other=1' },
 ]) {
  const h = await dashboard(uploadFetcher(() => response(receipt))); chooseUpload(h); await h.submitUpload(); await h.submitUpload();
  const finals = h.calls.filter(call => call.url.endsWith('/finalize')); assert.deepEqual(finals[0].body, finals[1].body); assert.ok(recoveryStates(h.storage)[0].finalize);
  assert.equal(h.calls.some(call => call.method === 'GET'), false); assert.equal(h.calls.filter(call => call.url.endsWith('/uploads')).length, 1);
 }
});

test('busy or unconfirmed trusted-host build failures retain the frozen finalize request', async () => {
 for (const code of ['build_busy', 'preparation_unconfirmed']) {
  const h = await dashboard(uploadFetcher(() => rejected(503, code, 'Build outcome is not yet confirmed.'))); chooseUpload(h); await h.submitUpload(); await h.submitUpload();
  const finals = h.calls.filter(call => call.url.endsWith('/finalize')); assert.deepEqual(finals[0].body, finals[1].body); assert.ok(recoveryStates(h.storage)[0].finalize); assert.equal(h.calls.filter(call => call.url.endsWith('/uploads')).length, 1);
 }
});

 test('malformed or mismatched build terminal responses retain the pending job', async () => {
 for (const status of [ {id:'job-original',share_id:'share-new',state:'ready'}, {id:'job-original',share_id:'share-new',status:'succeeded'}, {state:'failed'}, {state:'succeeded'}, {id:'other-job',share_id:'share-new',state:'failed'}, {id:'job-original',share_id:'other-share',state:'failed'}, {id:'job-original',share_id:'share-new',state:'succeeded',result:{}} ]) {
 const store=storage(), first=await dashboard(uploadFetcher(()=>{throw Error('Lost response');}),{storage:store}); chooseUpload(first); await first.submitUpload();
 const second=await dashboard(call=>call.url.endsWith('/finalize')?response(queued()):failure('Unavailable'),{storage:store}); await second.submitUpload();
 const third=await dashboard(()=>response(status),{storage:store}); await new Promise(resolve=>setImmediate(resolve));
 assert.ok(recoveryStates(store)[0].build); assert.equal(third.reloads,0);
 }
});
