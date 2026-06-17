// ironvein_audio.js — the browser half of IRONVEIN's audio.
//
// A second miniquad plugin (alongside ironvein_net.js). The Rust client owns
// WHEN and WHAT to play — it just maps game events to sound names and calls
// the ivn_audio_* FFI. JS owns the Web Audio plumbing: fetch + decode the
// procedural .wav/.ogg assets, manage one-shot and looping sources, and
// honour the browser's autoplay policy (an AudioContext stays suspended until
// the first user gesture, so we resume on the first click/keypress).

(function () {
  "use strict";

  let ctx = null;
  let master = null;
  const buffers = {}; // name -> decoded AudioBuffer
  const loops = {}; // name -> { src, gain, volume } currently-looping sources
  const pendingLoop = {}; // name -> volume, requested before the buffer decoded
  const lastPlay = {}; // name -> ctx.currentTime of last one-shot (anti-machine-gun)

  function ensureCtx() {
    if (!ctx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) return null;
      ctx = new AC();
      master = ctx.createGain();
      master.gain.value = 0.9;
      master.connect(ctx.destination);
    }
    if (ctx.state === "suspended") ctx.resume();
    return ctx;
  }

  // The autoplay policy: nothing is audible until a user gesture resumes the
  // context. Re-arm loops that were requested while we were muted.
  function unlock() {
    const c = ensureCtx();
    if (!c) return;
    for (const name in loops) {
      // a loop whose source already ended (suspended at create) — restart it
      if (loops[name].ended) startLoop(name, loops[name].volume);
    }
  }
  window.addEventListener("pointerdown", unlock);
  window.addEventListener("keydown", unlock);

  function loadAudio(name, url) {
    fetch(url)
      .then(function (r) { return r.arrayBuffer(); })
      .then(function (buf) {
        const c = ensureCtx();
        if (!c) throw new Error("no AudioContext");
        return c.decodeAudioData(buf);
      })
      .then(function (ab) {
        buffers[name] = ab;
        if (pendingLoop[name] !== undefined) {
          const v = pendingLoop[name];
          delete pendingLoop[name];
          startLoop(name, v);
        }
      })
      .catch(function (e) { console.warn("[ivn-audio] load failed: " + name, e); });
  }

  function startLoop(name, volume) {
    const c = ensureCtx();
    if (!c) return;
    const ab = buffers[name];
    if (!ab) { pendingLoop[name] = volume; return; }
    if (loops[name] && !loops[name].ended) { try { loops[name].src.stop(); } catch (e) {} }
    const src = c.createBufferSource();
    src.buffer = ab;
    src.loop = true;
    const gain = c.createGain();
    gain.gain.value = volume;
    src.connect(gain);
    gain.connect(master);
    const rec = { src: src, gain: gain, volume: volume, ended: false };
    src.onended = function () { rec.ended = true; };
    try { src.start(); } catch (e) {}
    loops[name] = rec;
  }

  function playOnce(name, volume) {
    const c = ensureCtx();
    if (!c) return;
    const ab = buffers[name];
    if (!ab) return;
    // dedupe a flood of identical one-shots in the same instant (phase stacking
    // turns ten overlapping explosions into clipping mush)
    const now = c.currentTime;
    if (lastPlay[name] !== undefined && now - lastPlay[name] < 0.04) return;
    lastPlay[name] = now;
    const src = c.createBufferSource();
    src.buffer = ab;
    const gain = c.createGain();
    gain.gain.value = volume;
    src.connect(gain);
    gain.connect(master);
    try { src.start(); } catch (e) {}
  }

  function stopLoop(name) {
    if (loops[name]) { try { loops[name].src.stop(); } catch (e) {} delete loops[name]; }
    delete pendingLoop[name];
  }

  function register(importObject) {
    const env = importObject.env;
    const str = function (p, l) { return new TextDecoder().decode(new Uint8Array(wasm_memory.buffer, p, l)); };
    env.ivn_audio_load = function (np, nl, up, ul) { loadAudio(str(np, nl), str(up, ul)); };
    env.ivn_audio_play = function (np, nl, vol, looping) {
      if (looping) startLoop(str(np, nl), vol);
      else playOnce(str(np, nl), vol);
    };
    env.ivn_audio_stop = function (np, nl) { stopLoop(str(np, nl)); };
    env.ivn_audio_master = function (vol) { if (master) master.gain.value = vol; };
    // live volume for a running loop (the settings sliders)
    env.ivn_audio_gain = function (np, nl, vol) {
      const name = str(np, nl);
      if (loops[name]) loops[name].gain.gain.value = vol;
      if (pendingLoop[name] !== undefined) pendingLoop[name] = vol;
    };

    // tiny persisted key/value store (settings) over localStorage
    env.ivn_pref_save = function (kp, kl, vp, vl) {
      try { localStorage.setItem("ivn_" + str(kp, kl), str(vp, vl)); } catch (e) {}
    };
    env.ivn_pref_load = function (kp, kl, out, cap) {
      try {
        const v = localStorage.getItem("ivn_" + str(kp, kl));
        if (v == null) return -1;
        const bytes = new TextEncoder().encode(v);
        if (bytes.length > cap) return -1;
        new Uint8Array(wasm_memory.buffer).set(bytes, out);
        return bytes.length;
      } catch (e) { return -1; }
    };
  }

  miniquad_add_plugin({
    register_plugin: register,
    on_init: function () {},
    version: "0.1.0",
    name: "ironvein_audio",
  });
})();
