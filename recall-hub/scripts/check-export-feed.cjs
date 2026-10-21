const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');

(async () => {
  const stores = await import('svelte/store');
  const source = fs.readFileSync(path.join(__dirname, '../src/lib/daemon.ts'), 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const calls = [];
  let receive;
  let resolveEdited;
  const editedCalls = [];
  const exported = { exports: {} };
  vm.runInNewContext(compiled, {
    exports: exported.exports, module: exported, Date, setTimeout() {},
    require(name) {
      if (name === 'svelte/store') return stores;
      if (name === '@tauri-apps/api/event') {
        return { listen: async (name, handler) => {
          assert.equal(name, 'daemon'); receive = handler; return () => {};
        } };
      }
      if (name === '@tauri-apps/api/core') {
        return { invoke: async (name, args) => {
          if (name === 'export_editor') {
            editedCalls.push(args);
            return new Promise(resolve => { resolveEdited = resolve; });
          }
          assert.equal(name, 'daemon_send'); calls.push(JSON.parse(args.line));
        } };
      }
      throw new Error(`unexpected dependency: ${name}`);
    },
  }, { filename: 'daemon.ts' });
  const api = exported.exports;
  const feed = (mod, event) => receive({ payload: JSON.stringify({ mod, event }) });
  await api.startDaemonFeed();
  await api.exportClip('source.mp4');
  await api.exportClip('source.mp4', 960);
  assert.deepEqual(calls, [
    { cmd: 'export_clip', path: 'source.mp4', fps: 60 },
    { cmd: 'export_clip', path: 'source.mp4', fps: 960 },
  ]);
  feed('engine', { type: 'export_started', source: 'source.mp4', path: 'export.mp4', fps: 960 });
  assert.equal(stores.get(api.exportStarted).fps, 960);
  feed('engine', { type: 'exported', source: 'source.mp4', path: 'export.mp4', fps: 960,
    frames: 212, seconds: 212 / 960, elapsed_ms: 20 });
  assert.equal(stores.get(api.exportStarted), null);
  assert.equal(stores.get(api.lastExported).path, 'export.mp4');
  feed('engine', { type: 'export_started', source: 'source.mp4', path: 'export2.mp4', fps: 60 });
  feed('hub', { type: 'disconnected' });
  assert.equal(stores.get(api.exportStarted), null);
  assert.match(stores.get(api.exportError), /connection lost/i);
  feed('engine', { type: 'export_failed', source: 'source.mp4', reason: 'NVENC unavailable' });
  assert.equal(stores.get(api.exportError), 'NVENC unavailable');
  feed('engine', { type: 'status', recording_mode: 'vfr', encoder: 'failed', encoder_error: 'driver failure' });
  assert.equal(stores.get(api.engine).recording_mode, 'vfr');
  assert.equal(stores.get(api.engine).encoder_error, 'driver failure');
  const plan = { ranges: [{ start: 0.4, end: 1.2 }], audio: [{ stream_index: 1, gain_db: -6, muted: false }] };
  api.exportStarted.set({ path: '', source: 'source.mp4', fps: 60, at: Date.now() });
  const editedJob = api.exportClip('source.mp4', 60, plan);
  feed('hub', { type: 'disconnected' });
  assert.equal(stores.get(api.exportStarted).source, 'source.mp4', 'offline editor export must keep its own busy state');
  assert.equal(editedCalls.length, 1);
  assert.equal(editedCalls[0].plan, plan);
  resolveEdited({ source: 'source.mp4', path: 'edited.mp4', fps: 60, frames: 48, seconds: .8, elapsed_ms: 2 });
  await editedJob;
  assert.equal(stores.get(api.lastExported).path, 'edited.mp4');
  assert.equal(stores.get(api.exportStarted), null);
  console.log('PASS: daemon export feed, offline edited export completion/disconnect, and NVENC error state.');
})().catch((error) => { console.error(error); process.exitCode = 1; });
