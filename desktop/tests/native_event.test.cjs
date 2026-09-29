// SPDX-License-Identifier: AGPL-3.0-only
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const assert = require('node:assert/strict');

const source = fs.readFileSync(path.join(__dirname, '../ui/app.js'), 'utf8');
const start = source.indexOf('window.__nativeEvent=');
const end = source.indexOf('\nasync function call(', start);
assert.ok(start >= 0 && end > start, 'native event handler must remain locatable');
const handler = source.slice(start, end);

function harness() {
  const calls = [];
  const timers = [];
  const elements = new Map([
    ['modal-layer', {hidden: true}],
    ['update-status', {textContent: ''}],
    ['tab-drag-hint', {hidden: true, textContent: '', style: {}}],
  ]);
  const tab = {loadToken: 'token', entries: [{name: 'old'}], dirty: false};
  const otherTab = {loadToken: 'other', entries: [], dirty: false};
  let activeTab = tab;
  let timerId = 0;
  const state = {
    cacheTimer: null,
    tabs: [tab, otherTab],
    env: {systemDark: false},
    theme: 'system',
    updateInstalling: true,
    signedOutHosts: new Set(),
    sessionNetwork: [{uri: 'smb://server/share'}, {uri: 'smb://other/share'}],
    query: 'notes',
  };
  const context = {
    window: {},
    innerWidth: 1000,
    innerHeight: 700,
    URL,
    state,
    activeAuth: null,
    setTimeout(fn, delay) {
      const timer = {id: ++timerId, fn, delay};
      timers.push(timer);
      return timer.id;
    },
    clearTimeout(id) {
      calls.push(['clearTimeout', id]);
    },
    $: id => elements.get(id),
    active: () => activeTab,
    scheduleListing: () => calls.push(['scheduleListing']),
    ...Object.fromEntries([
      'beginNativeFileDrag', 'markNativeFileDrag', 'finishNativeFileDrag',
      'receiveFileDrop', 'showFileDropHint', 'restoreTransferredTab',
      'beginNativeTabDrag', 'detachTab', 'receiveTransferredTab',
      'finishTabTransfer', 'settleIncomingTab', 'reorderTab', 'showTabDropHint',
      'handleFileManagerRequest', 'windowsMenu', 'settingsDialog',
      'updateDefaultStatus', 'receiveFolderSize', 'receiveAuth', 'dismissAuth',
      'goHistory', 'openIncoming', 'refreshClipboard', 'applyTextSize',
      'updateTransfer', 'refreshEnvironment', 'askClose',
    ].map(name => [name, (...args) => calls.push([name, ...args])])),
    refreshCacheStatus: async () => calls.push(['refreshCacheStatus']),
    runSearch: () => calls.push(['runSearch']),
    applyTheme: (...args) => calls.push(['applyTheme', ...args]),
    load: (...args) => calls.push(['load', ...args]),
    toast: (...args) => calls.push(['toast', ...args]),
  };
  vm.runInNewContext(handler, vm.createContext(context), {filename: 'app.js'});
  return {context, calls, timers, elements, state, tab, otherTab, setActive: tabValue => { activeTab = tabValue; }};
}

test('native event dispatch preserves simple calls, aliases, and unknown events', () => {
  const h = harness();
  const dispatch = h.context.window.__nativeEvent;
  dispatch('fileDragRequest', {uri: 'file:///tmp/demo'});
  dispatch('tabReorder', {id: 'tab-1', beforeId: 'tab-2'});
  dispatch('environmentChanged', {});
  dispatch('mounts', {});
  dispatch('notice', {message: 'hello'});
  const beforeUnknown = h.calls.length;
  assert.equal(dispatch('futureEvent', {}), undefined);
  assert.equal(h.calls.length, beforeUnknown);
  assert.deepEqual(h.calls, [
    ['beginNativeFileDrag', 'file:///tmp/demo'],
    ['reorderTab', 'tab-1', 'tab-2'],
    ['refreshEnvironment'],
    ['refreshEnvironment'],
    ['toast', 'hello'],
  ]);
  assert.equal(dispatch('fileDragFinished', {}), undefined);
});

test('native event guards and multi-step state updates stay intact', () => {
  const h = harness();
  const dispatch = h.context.window.__nativeEvent;
  dispatch('entries', {token: 'wrong', reset: true, entries: [{name: 'ignored'}]});
  assert.equal(JSON.stringify(h.tab.entries.map(entry => entry.name)), '["old"]');
  dispatch('entries', {token: 'token', reset: true, entries: [{name: 'new'}]});
  assert.equal(JSON.stringify(h.tab.entries.map(entry => entry.name)), '["new"]');
  assert.equal(h.tab.dirty, true);
  assert.deepEqual(h.calls, [['scheduleListing']]);

  h.setActive(h.otherTab);
  dispatch('entries', {token: 'token', entries: [{name: 'later'}]});
  assert.equal(JSON.stringify(h.tab.entries.map(entry => entry.name)), '["new","later"]');
  assert.deepEqual(h.calls, [['scheduleListing']]);

  dispatch('mouseNavigate', {delta: 1});
  h.context.activeAuth = {};
  dispatch('mouseNavigate', {delta: 2});
  h.context.activeAuth = null;
  h.elements.get('modal-layer').hidden = false;
  dispatch('mouseNavigate', {delta: 3});
  assert.deepEqual(h.calls, [['scheduleListing'], ['goHistory', 1]]);

  dispatch('updateProgress', {message: 'working'});
  assert.equal(h.elements.get('update-status').textContent, 'working');
  h.state.updateInstalling = false;
  dispatch('updateProgress', {message: 'ignored'});
  assert.equal(h.elements.get('update-status').textContent, 'working');
});

test('cache event clears the old timer before preserving callback order', async () => {
  const h = harness();
  const dispatch = h.context.window.__nativeEvent;
  h.state.cacheTimer = 41;
  dispatch('cacheChanged', {});
  assert.deepEqual(h.calls, [['clearTimeout', 41]]);
  assert.equal(h.state.cacheTimer, 1);
  assert.deepEqual(h.timers.map(timer => timer.delay), [200]);
  await h.timers[0].fn();
  assert.deepEqual(h.calls, [['clearTimeout', 41], ['refreshCacheStatus'], ['runSearch']]);
});
