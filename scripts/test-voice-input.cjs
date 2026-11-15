// Data-only regression tests for the actual voice page. No browser, mic or sound.
// Run: node --test scripts/test-voice-input.cjs
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

const source = readFileSync(new URL('../src/voice/page.html', `file://${__filename}`), 'utf8')
  .match(/<script>([\s\S]*?)<\/script>/)[1];

function page({ permission = true, disappear = false, switchDuringPermission = false } = {}) {
  let current = 'airpods', allowed = permission;
  const calls = [], tracks = [], events = {}, sources = [], senders = [];
  let connected = ['airpods', 'laptop'], pendingCapture;
  const names = { airpods: 'AirPods (Bluetooth)', laptop: 'MacBook Pro Microphone (Built-in)' };
  const devices = () => [
    { deviceId: 'default', groupId: current, label: `Default - ${names[current]}` },
    ...Object.entries(names).filter(([id]) => connected.includes(id)).map(([deviceId, label]) => ({ deviceId, groupId: deviceId, label })),
  ].map((d) => ({ ...d, kind: 'audioinput', ...(!allowed ? { groupId: '', label: '' } : {}) }));
  class AudioContext {
    constructor() { this.audioWorklet = { addModule: async () => {} }; }
    async resume() {} async close() {}
    createMediaStreamSource(media) {
      const source = { media, connected: false, connect() { this.connected = true; }, disconnect() { this.connected = false; } };
      sources.push(source);
      return source;
    }
    createAnalyser() { return {}; }
  }
  class Peer {
    constructor() { this.iceGatheringState = 'complete'; }
    addTrack(track) { senders.push({ track, async replaceTrack(next) { this.track = next; } }); }
    getSenders() { return senders; }
    createDataChannel() { return { close() {} }; }
    async createOffer() { return {}; }
    async setLocalDescription(sdp) { this.localDescription = { sdp }; }
    close() {}
  }
  const elements = new Map();
  const context = vm.createContext({
    URLSearchParams, DOMException, Float32Array, Blob,
    URL: { createObjectURL: () => 'blob:test' },
    location: { search: '?hidden', host: 'localhost' },
    document: {
      getElementById: (id) => {
        if (!elements.has(id)) elements.set(id, { style: {}, remove() {} });
        return elements.get(id);
      },
      addEventListener() {},
    },
    navigator: { mediaDevices: {
      enumerateDevices: async () => devices(),
      addEventListener: (event, fn) => { events[event] = fn; },
      getUserMedia: async ({ audio }) => {
        calls.push(audio);
        const id = audio.deviceId?.exact;
        if (pendingCapture && id) { const wait = pendingCapture; pendingCapture = null; await wait; }
        if (id && disappear) throw new DOMException('Device gone', 'OverconstrainedError');
        const opened = id || current;
        const track = {
          label: id ? names[id] : `Default - ${names[current]}`,
          readyState: 'live', enabled: true, kind: 'audio',
          getSettings: () => ({ deviceId: id || 'default', groupId: opened }),
          getConstraints: () => audio,
          // Model the bug: default capture follows routing, exact capture doesn't.
          hearing: () => id || current,
          stop() { this.readyState = 'ended'; },
          addEventListener(event, fn) { this[event] = fn; },
        };
        tracks.push(track);
        allowed = true;
        if (!id && switchDuringPermission) current = 'laptop';
        return { getAudioTracks: () => [track], getTracks: () => [track] };
      },
    } },
    WebSocket: class { send() {} }, AudioContext, RTCPeerConnection: Peer,
    AudioWorkletNode: class { constructor() { this.port = {}; } disconnect() {} },
    setInterval() {}, clearInterval() {}, setTimeout() {},
  });
  return {
    run: (code) => vm.runInContext(code, context), calls, tracks, sources, senders,
    switchInput: async () => { current = 'laptop'; await events.devicechange(); },
    disconnect: async () => {
      connected = ['laptop']; current = 'laptop';
      tracks.filter((t) => t.hearing() === 'airpods' && t.readyState === 'live').forEach((t) => { t.readyState = 'ended'; t.ended?.(); });
      await events.devicechange();
    },
    reconnect: async () => { connected = ['airpods', 'laptop']; await events.devicechange(); },
    removeAll: async () => { connected = []; await events.devicechange(); },
    holdCapture: () => { let release; pendingCapture = new Promise((r) => { release = r; }); return release; },
    start: async (provider) => { vm.runInContext(source, context); await vm.runInContext(provider === 'xai' ? 'beginXai()' : 'begin()', context); },
  };
}

for (const provider of ['openai', 'xai']) {
  test(`${provider}: automation changes the default without moving call capture`, async () => {
    const p = page();
    await p.start(provider);
    assert.equal(p.calls.length, 1);
    assert.equal(p.calls[0].deviceId.exact, 'airpods');
    await p.switchInput();
    assert.equal(p.run('stream.getAudioTracks()[0].hearing()'), 'airpods');
    assert.equal(p.calls.length, 1, 'devicechange must not reopen the new default');
    p.run('muted = true; applyMute()');
    assert.equal(p.tracks[0].enabled, false);
    p.run('end("test")');
    assert.equal(p.tracks[0].readyState, 'ended');
  });

  test(`${provider}: first permission pins the opened mic even if default changes`, async () => {
    const p = page({ permission: false, switchDuringPermission: true });
    await p.start(provider);
    assert.equal(p.calls.length, 2);
    assert.equal(p.calls[1].deviceId.exact, 'airpods');
    assert.equal(p.tracks[0].readyState, 'ended', 'permission probe must be released');
    assert.equal(p.run('stream.getAudioTracks()[0].hearing()'), 'airpods');
    p.run('end("test")');
  });

  test(`${provider}: missing device fails instead of falling back to laptop`, async () => {
    const p = page({ disappear: true });
    await p.start(provider);
    assert.equal(p.calls.length, 1);
    assert.equal(p.run('!!stream'), false);
  });

  test(`${provider}: disconnect falls back, reconnect restores preference and mute`, async () => {
    const p = page();
    await p.start(provider);
    p.run('muted = true; applyMute()');
    await p.disconnect();
    assert.equal(p.run('stream.getAudioTracks()[0].hearing()'), 'laptop');
    assert.equal(p.run('stream.getAudioTracks()[0].enabled'), false);
    if (provider === 'openai') assert.equal(p.senders[0].track.hearing(), 'laptop');
    else {
      assert.equal(p.run('captureSource.media.getAudioTracks()[0].hearing()'), 'laptop');
      assert.equal(p.sources[1].connected, false, 'old PCM source must be disconnected');
    }
    await p.reconnect();
    assert.equal(p.run('stream.getAudioTracks()[0].hearing()'), 'airpods');
    assert.equal(p.tracks[1].readyState, 'ended', 'fallback capture must be released');
    if (provider === 'openai') assert.equal(p.senders[0].track.hearing(), 'airpods');
    else assert.equal(p.run('captureSource.media.getAudioTracks()[0].hearing()'), 'airpods');
    p.run('end("test")');
  });

  test(`${provider}: ending during recovery releases late capture`, async () => {
    const p = page();
    await p.start(provider);
    const release = p.holdCapture();
    const disconnected = p.disconnect();
    // Flush the enumerateDevices await so recovery is inside getUserMedia.
    await new Promise(setImmediate);
    p.run('end("test")');
    release();
    await disconnected;
    assert.equal(p.run('stream'), null);
    assert.ok(p.tracks.every((t) => t.readyState === 'ended'));
  });

  test(`${provider}: reconnect after no available inputs recovers the same call`, async () => {
    const p = page();
    await p.start(provider);
    await p.removeAll();
    assert.equal(p.calls.length, 1, 'no default alias capture while devices are absent');
    await p.disconnect();
    assert.equal(p.run('stream.getAudioTracks()[0].hearing()'), 'laptop');
    p.run('end("test")');
  });

  test(`${provider}: reconnect during a pending fallback is not lost`, async () => {
    const p = page();
    await p.start(provider);
    const release = p.holdCapture();
    const disconnected = p.disconnect();
    await new Promise(setImmediate);
    const reconnected = p.reconnect();
    release();
    await Promise.all([disconnected, reconnected]);
    assert.equal(p.run('stream.getAudioTracks()[0].hearing()'), 'airpods');
    assert.equal(p.tracks[1].readyState, 'ended');
    p.run('end("test")');
  });
}
