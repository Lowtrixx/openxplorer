// SPDX-License-Identifier: AGPL-3.0-only
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const assert = require('node:assert/strict');

const source = fs.readFileSync(path.join(__dirname, '../ui/app.js'), 'utf8');

function extract(startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${startMarker} must remain locatable`);
  return source.slice(start, end);
}

function operationHarness() {
  const sourceTab = {id: 'source'};
  const targetTab = {id: 'target'};
  const transfer = {hidden: true};
  const loads = [];
  const state = {operation: null, selection: new Set(['source-file']), tabs: [sourceTab, targetTab]};
  let activeTab = sourceTab;
  let resolveOperation;
  const context = {
    state,
    active: () => activeTab,
    updateToolbar: () => {},
    updateTransfer: () => {},
    $: id => id === 'transfer' ? transfer : null,
    call: () => new Promise(resolve => { resolveOperation = resolve; }),
    showMessage: () => {},
    toast: () => {},
    load: (tab, clear) => { loads.push([tab, clear]); return Promise.resolve(); },
  };
  vm.runInNewContext(`let seq=0;${extract('async function runOperation(', '\nfunction updateTransfer')}`, vm.createContext(context), {filename: 'app.js'});
  return {context, state, sourceTab, targetTab, loads, switchTo: tab => { activeTab = tab; }, finish: result => resolveOperation(result)};
}

test('operation completion reloads its source tab and preserves another tab selection', async () => {
  const h = operationHarness();
  const operation = h.context.runOperation('copy', {uris: ['source-file'], target: 'target'});
  h.switchTo(h.targetTab);
  h.state.selection = new Set(['target-file']);
  h.finish({done: [], errors: []});
  await operation;
  assert.deepEqual(h.loads, [[h.sourceTab, false]]);
  assert.deepEqual([...h.state.selection], ['target-file']);
});

function cacheHarness() {
  const pending = [];
  const rendered = [];
  const state = {cache: {version: 0}, cacheStatusGeneration: 0, cacheError: ''};
  const context = {
    state,
    call: () => new Promise((resolve, reject) => pending.push({resolve, reject})),
    renderSearchInfo: () => rendered.push({...state.cache}),
    renderSettingsCache: () => {},
    $: () => null,
  };
  vm.runInNewContext(extract('async function refreshCacheStatus(', '\nfunction queueSearch'), vm.createContext(context), {filename: 'app.js'});
  return {context, state, pending, rendered};
}

test('cache status keeps the newest overlapping response', async () => {
  const h = cacheHarness();
  const older = h.context.refreshCacheStatus();
  const newer = h.context.refreshCacheStatus();
  h.pending[1].resolve({version: 2});
  await newer;
  h.pending[0].resolve({version: 1});
  await older;
  assert.deepEqual(h.state.cache, {version: 2});
  assert.deepEqual(h.rendered, [{version: 2}]);
});

test('windows menu consumes bridge errors from event and click callers', async () => {
  const toasts = [];
  let opened = false;
  const context = {
    call: async () => { throw Error('window list unavailable'); },
    toast: message => toasts.push(message),
    openMenu: () => { opened = true; },
    $: () => ({getBoundingClientRect: () => ({left: 0, bottom: 0})}),
    innerWidth: 1000,
    state: {env: {home: 'file:///home/demo'}},
  };
  vm.runInNewContext(extract('async function windowsMenu(', '\nasync function handleFileManagerRequest'), vm.createContext(context), {filename: 'app.js'});
  await context.windowsMenu();
  assert.deepEqual(toasts, ['window list unavailable']);
  assert.equal(opened, false);
});
