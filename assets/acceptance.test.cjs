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
const thread = (id, body = 'Original comment') => ({ id, original_revision: 'revision-1', original_anchor: { kind: 'general' }, resolved: false, comments: [{ id: `${id}-comment`, body, display_name: 'Reviewer', authenticated: false }] });
const response = data => ({ ok: true, status: 200, json: async () => structuredClone(data) });
const failure = message => ({ ok: false, status: 503, json: async () => ({ message }) });
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
function harness(initialThreads, fetcher) {
 const body = new Element('body'), app = new Element(); app.id = 'app'; app.dataset.page = 'review'; app.dataset.boot = JSON.stringify({ capability: 'review-token', revision: { id: 'revision-1' }, share: { id: 'share-1' }, can_manage: true, threads: initialThreads }); body.append(app);
 const ids = ['guest-name', 'artifact-frame', 'anchor-preview', 'composer-label', 'selection-action', 'discussion-panel', 'comment-body', 'select-element', 'selection-hint', 'comment-selection', 'cancel-anchor', 'general-comment', 'new-comment', 'toggle-discussion', 'copy-review-link', 'thread-list', 'thread-count', 'refresh-discussion', 'comment-form', 'comment-error', 'toast'];
 ids.forEach(id => { const node = new Element(id === 'comment-form' ? 'form' : id === 'comment-body' ? 'textarea' : 'div'); node.id = id; app.append(node); });
 const identity = new Element('p'); identity.className = 'identity-note'; app.append(identity);
 const filters = {};
 for (const name of ['open', 'resolved', 'unmatched']) { const button = new Element('button'); button.dataset.threadFilter = name; app.append(button); filters[name] = button; }
 const $ = selector => body.querySelector(selector);
 $('#guest-name').value = 'Guest'; $('#artifact-frame').contentWindow = { postMessage() {} };
 const submit = new Element('button'); submit.type = 'submit'; $('#comment-form').append(submit);
 const document = { querySelector: $, querySelectorAll: selector => body.querySelectorAll(selector), getElementById: id => $(`#${id}`), createElement: tag => new Element(tag), addEventListener() {} };
 const calls = [];
 const context = { document, window: { addEventListener() {} }, crypto: webcrypto, fetch: async (url, options = {}) => { const call = { url: String(url), method: options.method || 'GET', body: options.body && JSON.parse(options.body) }; calls.push(call); return fetcher(call); }, location: { origin: 'http://localhost', href: 'http://localhost/s/review-token' }, navigator: {}, setTimeout: () => 1, clearTimeout() {}, matchMedia: () => ({ matches: false }), console };
 vm.runInNewContext(source, context, { timeout: 1000 });
 const card = id => $('#thread-list').querySelectorAll('.thread-card').find(node => node.dataset.id === id);
 const action = (id, label) => card(id).querySelectorAll('button').find(node => node.textContent === label);
 const openReply = id => { action(id, 'Reply').onclick(); return card(id).querySelector('.reply-form'); };
 const submitReply = id => card(id).querySelector('.reply-form').onsubmit({ preventDefault() {} });
 return { $, filters, calls, card, action, openReply, submitReply };
}

test('a saved reply is reconciled from POST without a fallible follow-up GET or duplicate retry', async () => {
 const original = thread('one'), saved = { ...original, comments: [...original.comments, { id: 'reply-1', body: 'Saved reply', display_name: 'Guest' }] };
 const h = harness([original], call => call.method === 'POST' ? response(saved) : failure('Refresh unavailable'));
 h.openReply('one').querySelector('textarea').value = 'Saved reply';
 await h.submitReply('one');
 assert.equal(h.calls.length, 1); assert.equal(h.calls[0].method, 'POST');
 assert.match(h.card('one').textContent, /Saved reply/); assert.equal(h.card('one').querySelector('.reply-form'), null);
 assert.equal(h.$('#toast').textContent, 'Reply posted');
 h.openReply('one'); assert.equal(h.card('one').querySelector('textarea').value, '');
});

test('unsent reply drafts survive discussion refresh and filter changes', async () => {
 const original = thread('one'), h = harness([original], () => response([original]));
 h.openReply('one').querySelector('textarea').value = 'Keep this draft';
 await h.$('#refresh-discussion').onclick();
 assert.equal(h.card('one').querySelector('textarea').value, 'Keep this draft');
 h.filters.resolved.onclick(); assert.equal(h.card('one'), undefined);
 h.filters.open.onclick(); assert.equal(h.card('one').querySelector('textarea').value, 'Keep this draft');
});

test('failed reply keeps its draft and inline error across filters, then retries once', async () => {
 const original = thread('one'); let attempts = 0;
 const h = harness([original], () => ++attempts === 1 ? failure('Try again later') : response({ ...original, comments: [...original.comments, { body: 'Retry me' }] }));
 h.openReply('one').querySelector('textarea').value = 'Retry me'; await h.submitReply('one');
 let form = h.card('one').querySelector('.reply-form'); assert.equal(form.querySelector('textarea').value, 'Retry me'); assert.equal(form.querySelector('button').disabled, false); assert.match(form.textContent, /Try again later/);
 h.filters.resolved.onclick(); h.filters.open.onclick(); form = h.card('one').querySelector('.reply-form'); assert.match(form.textContent, /Try again later/);
 await h.submitReply('one'); assert.equal(attempts, 2); assert.equal(h.card('one').querySelector('.reply-form'), null);
});

test('pending reply stays disabled across rerenders and repeated submission cannot duplicate it', async () => {
 const original = thread('one'), pending = deferred(), h = harness([original], () => pending.promise);
 h.openReply('one').querySelector('textarea').value = 'Only once'; const posting = h.submitReply('one');
 h.filters.resolved.onclick(); h.filters.open.onclick();
 const form = h.card('one').querySelector('.reply-form'); assert.equal(form.querySelector('button').disabled, true); assert.equal(form.querySelector('textarea').disabled, true);
 await h.submitReply('one'); assert.equal(h.calls.length, 1);
 pending.resolve(response({ ...original, comments: [...original.comments, { body: 'Only once' }] })); await posting;
 assert.equal(h.card('one').querySelector('.reply-form'), null);
});

test('posting one reply preserves a different thread’s draft', async () => {
 const one = thread('one'), two = thread('two'), h = harness([one, two], () => response({ ...one, comments: [...one.comments, { body: 'Posted one' }] }));
 h.openReply('one').querySelector('textarea').value = 'Posted one'; h.openReply('two').querySelector('textarea').value = 'Unsent two';
 await h.submitReply('one'); assert.equal(h.card('two').querySelector('textarea').value, 'Unsent two');
});

test('an older in-flight refresh cannot remove a newly acknowledged reply', async () => {
 const original = thread('one'), stale = deferred(), h = harness([original], call => call.method === 'GET' ? stale.promise : response({ ...original, comments: [...original.comments, { body: 'Newest reply' }] }));
 const refreshing = h.$('#refresh-discussion').onclick(); h.openReply('one').querySelector('textarea').value = 'Newest reply'; await h.submitReply('one');
 stale.resolve(response([original])); await refreshing; assert.match(h.card('one').textContent, /Newest reply/);
});

test('resolving a thread does not discard its unfinished reply', async () => {
 const original = thread('one'), resolved = { ...original, resolved: true }, h = harness([original], call => response(call.method === 'POST' ? resolved : [resolved]));
 h.openReply('one').querySelector('textarea').value = 'Return to this'; await h.action('one', 'Resolve').onclick();
 assert.equal(h.card('one'), undefined); h.filters.resolved.onclick(); assert.equal(h.card('one').querySelector('textarea').value, 'Return to this');
});
