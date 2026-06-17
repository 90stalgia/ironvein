// ironvein_net.js — the browser half of IRONVEIN's serverless networking.
//
// A miniquad plugin (macroquad runs on miniquad) that gives the Rust/wasm
// core two dumb pipes and nothing more:
//   * a pool of Nostr relay WebSockets (wss://…), and
//   * a mesh of RTCPeerConnections / RTCDataChannels.
//
// It speaks NO crypto and makes NO decisions. Rust signs every Nostr event,
// ECDH-encrypts every SDP/ICE blob, verifies every inbound frame, and runs
// the whole lockstep simulation. JS just moves bytes and queues them for
// Rust to poll — matching the sans-io `Transport`/`RelayClient` traits.
//
// FFI mirrors crates/net/src/transport_webrtc.rs exactly.

(function () {
  "use strict";

  // ---- wasm memory helpers (miniquad exposes `wasm_memory`) -------------
  function mem() {
    return new Uint8Array(wasm_memory.buffer);
  }
  function readStr(ptr, len) {
    return new TextDecoder().decode(new Uint8Array(wasm_memory.buffer, ptr, len));
  }
  function readBytes(ptr, len) {
    return new Uint8Array(wasm_memory.buffer, ptr, len).slice();
  }
  // Copy `bytes` into the Rust-provided [out, out+cap) buffer; return length
  // or -1 if it doesn't fit.
  function writeOut(out, cap, bytes) {
    if (bytes.length > cap) return -1;
    mem().set(bytes, out);
    return bytes.length;
  }
  function hexToBytes(hex) {
    const b = new Uint8Array(hex.length / 2);
    for (let i = 0; i < b.length; i++) b[i] = parseInt(hex.substr(i * 2, 2), 16);
    return b;
  }

  // ---- relay pool -------------------------------------------------------
  const relays = [];
  const relayInbox = []; // strings (raw relay frames)
  const pendingSends = []; // frames queued before any socket is open
  const rejectSeen = {}; // "<url>|<reason>" -> already-warned (hush repeats)

  function relayConnect(url) {
    let ws;
    try {
      ws = new WebSocket(url);
    } catch (e) {
      console.warn("[ivn] bad relay url", url, e);
      return;
    }
    ws.onopen = function () {
      console.log("[ivn] relay OPEN " + url);
      // flush anything queued while connecting (REQ subscriptions, etc.)
      for (const f of pendingSends) {
        try { ws.send(f); } catch (e) {}
      }
    };
    ws.onmessage = function (ev) {
      const data = typeof ev.data === "string" ? ev.data : "";
      relayInbox.push(data);
      if (data.startsWith('["NOTICE"') || (data.startsWith('["OK"') && data.indexOf("false") !== -1)) {
        // relays that reject our writes (rate-limit / PoW / web-of-trust) say
        // so on every beacon — log the reason once per relay, then hush.
        const reason = (/,"([^"]*)"\]\s*$/.exec(data) || [])[1] || data.slice(0, 80);
        const tag = url + "|" + reason.split(":")[0];
        if (!rejectSeen[tag]) {
          rejectSeen[tag] = true;
          console.warn("[ivn] relay @" + url.replace("wss://", "") + " rejects writes: " + reason + " (silencing repeats)");
        }
      }
    };
    ws.onclose = function () { console.log("[ivn] relay closed " + url); };
    ws.onerror = function () { console.warn("[ivn] relay error " + url); };
    relays.push(ws);
  }

  function relaySend(frame) {
    let sent = false;
    for (const ws of relays) {
      if (ws.readyState === WebSocket.OPEN) {
        try { ws.send(frame); sent = true; } catch (e) {}
      }
    }
    if (!sent) pendingSends.push(frame); // resend on next open
  }

  // ---- WebRTC mesh ------------------------------------------------------
  const ICE = {
    iceServers: [
      { urls: "stun:stun.l.google.com:19302" },
      { urls: "stun:stun1.l.google.com:19302" },
    ],
  };
  let nextConn = 1;
  const byKey = {}; // pubkeyHex -> peer record
  const byConn = {}; // conn id  -> peer record
  const sigOut = []; // {peer: Uint8Array(32), kind: 0|1|2, body: string}
  const evtOut = []; // {conn, type: 0|1|2, bytes: Uint8Array}

  function makePeer(pubkeyHex) {
    const pc = new RTCPeerConnection(ICE);
    const rec = {
      pc, dc: null, conn: nextConn++, key: pubkeyHex, keyBytes: hexToBytes(pubkeyHex),
      // ICE that arrives before setRemoteDescription must be queued, or
      // addIceCandidate rejects and the connection silently never forms.
      remoteSet: false, pendingIce: [],
      // Frames the Rust session sends before the data channel is "open" (the
      // Joiner fires its Hello the instant it dials) — buffer and flush on
      // open, since the sans-io layer assumes the transport queues for it.
      sendQueue: [],
    };
    byKey[pubkeyHex] = rec;
    byConn[rec.conn] = rec;

    pc.onicecandidate = function (e) {
      if (e.candidate) {
        sigOut.push({ peer: rec.keyBytes, kind: 2, body: JSON.stringify(e.candidate) });
      }
    };
    pc.ondatachannel = function (e) {
      bindChannel(rec, e.channel);
    };
    pc.onconnectionstatechange = function () {
      if (pc.connectionState === "failed" || pc.connectionState === "closed") {
        if (!rec.closedByUs) {
          console.warn("[ivn] peer " + pubkeyHex.slice(0, 8) + " connection lost (" + pc.connectionState + ")");
        }
        evtOut.push({ conn: rec.conn, type: 2, bytes: new Uint8Array(0) });
      }
    };
    return rec;
  }

  // Apply any ICE candidates that arrived before the remote description.
  function flushIce(rec) {
    const q = rec.pendingIce;
    rec.pendingIce = [];
    for (const cand of q) {
      rec.pc.addIceCandidate(cand).catch(function (e) {
        console.warn("[ivn] late addIce failed", e);
      });
    }
  }

  function bindChannel(rec, dc) {
    rec.dc = dc;
    dc.binaryType = "arraybuffer";
    dc.onopen = function () {
      console.log("[ivn] peer " + rec.key.slice(0, 8) + " connected");
      // flush anything the session tried to send before the channel was ready
      for (const b of rec.sendQueue) {
        try { dc.send(b); } catch (e) {}
      }
      rec.sendQueue = [];
      evtOut.push({ conn: rec.conn, type: 0, bytes: new Uint8Array(0) });
    };
    dc.onmessage = function (e) {
      const bytes = e.data instanceof ArrayBuffer ? new Uint8Array(e.data) : new TextEncoder().encode(e.data);
      evtOut.push({ conn: rec.conn, type: 1, bytes });
    };
    dc.onclose = function () {
      evtOut.push({ conn: rec.conn, type: 2, bytes: new Uint8Array(0) });
    };
  }

  // We are the OFFERER: create the data channel and an offer.
  function rtcDial(pubkeyHex) {
    let rec = byKey[pubkeyHex];
    if (!rec) rec = makePeer(pubkeyHex);
    if (!rec.dc) bindChannel(rec, rec.pc.createDataChannel("iv", { ordered: true }));
    rec.pc.createOffer().then(function (offer) {
      return rec.pc.setLocalDescription(offer).then(function () {
        sigOut.push({ peer: rec.keyBytes, kind: 0, body: offer.sdp });
      });
    }).catch(function (e) { console.warn("[ivn] offer failed", e); });
    return rec.conn;
  }

  // Idempotent: return the existing PC for a peer, or stand up an answerer.
  function rtcAccept(pubkeyHex) {
    let rec = byKey[pubkeyHex];
    if (!rec) rec = makePeer(pubkeyHex);
    return rec.conn;
  }

  function rtcSetRemote(conn, kind, sdp) {
    const rec = byConn[conn];
    if (!rec) return;
    const type = kind === 0 ? "offer" : "answer";
    // Belt-and-suspenders against duplicate signaling (the Rust lobby also
    // dedups by event id): an answer is only valid in have-local-offer state;
    // re-applying it once stable throws InvalidStateError.
    if (kind === 1 && rec.pc.signalingState === "stable") {
      return;
    }
    rec.pc.setRemoteDescription({ type, sdp }).then(function () {
      rec.remoteSet = true;
      flushIce(rec); // drain candidates that beat the description here
      if (kind === 0) {
        // we received an offer -> answer it
        return rec.pc.createAnswer().then(function (ans) {
          return rec.pc.setLocalDescription(ans).then(function () {
            sigOut.push({ peer: rec.keyBytes, kind: 1, body: ans.sdp });
          });
        });
      }
    }).catch(function (e) { console.warn("[ivn] setRemote failed", e); });
  }

  function rtcAddIce(conn, candJson) {
    const rec = byConn[conn];
    if (!rec) return;
    let cand;
    try { cand = JSON.parse(candJson); } catch (e) { return; }
    if (!rec.remoteSet) {
      rec.pendingIce.push(cand); // queue until the remote description lands
      return;
    }
    rec.pc.addIceCandidate(cand).catch(function () {});
  }

  function rtcSend(conn, bytes) {
    const rec = byConn[conn];
    if (!rec) return;
    if (rec.dc && rec.dc.readyState === "open") {
      try { rec.dc.send(bytes); } catch (e) {}
    } else {
      // channel not up yet — hold it (bytes is already a copy from readBytes)
      rec.sendQueue.push(bytes);
    }
  }

  function rtcClose(conn) {
    const rec = byConn[conn];
    if (!rec) return;
    rec.closedByUs = true; // distinguish a deliberate close from an ICE drop
    try { rec.pc.close(); } catch (e) {}
    delete byConn[conn];
    delete byKey[rec.key];
  }

  // ---- the plugin: wire these into the wasm import table ----------------
  function register(importObject) {
    const env = importObject.env;

    env.ivn_random = function (ptr, len) {
      crypto.getRandomValues(new Uint8Array(wasm_memory.buffer, ptr, len));
    };
    env.ivn_now_ms = function () {
      return Date.now();
    };

    // Read a `?key=value` query param off the page URL into the Rust scratch
    // buffer. Returns the byte length written, or -1 if the param is absent
    // (or doesn't fit). This is how the browser picks region/name/color:
    //   …/index.html?region=B2&name=Ada&color=3
    env.ivn_url_param = function (kptr, klen, out, cap) {
      try {
        const key = readStr(kptr, klen);
        const val = new URLSearchParams(window.location.search).get(key);
        if (val == null || val === "") return -1;
        return writeOut(out, cap, new TextEncoder().encode(val));
      } catch (e) {
        return -1;
      }
    };

    // Browser desktop notification (pulls the player back when the tab is in
    // the background). Requests permission lazily on first use.
    env.ivn_notify = function (tptr, tlen, bptr, blen) {
      var title = readStr(tptr, tlen);
      var body = readStr(bptr, blen);
      try {
        if (typeof Notification === "undefined") return;
        if (Notification.permission === "granted") {
          new Notification(title, { body: body });
        } else if (Notification.permission !== "denied") {
          Notification.requestPermission().then(function (p) {
            if (p === "granted") new Notification(title, { body: body });
          });
        }
      } catch (e) {}
    };

    env.ivn_relay_connect = function (ptr, len) { relayConnect(readStr(ptr, len)); };
    env.ivn_relay_send = function (ptr, len) { relaySend(readStr(ptr, len)); };
    env.ivn_relay_poll = function (out, cap) {
      if (relayInbox.length === 0) return -1;
      return writeOut(out, cap, new TextEncoder().encode(relayInbox.shift()));
    };

    env.ivn_rtc_dial = function (ptr, len) { return rtcDial(readStr(ptr, len)); };
    env.ivn_rtc_accept = function (ptr, len) { return rtcAccept(readStr(ptr, len)); };
    env.ivn_rtc_send = function (conn, ptr, len) { rtcSend(conn, readBytes(ptr, len)); };
    env.ivn_rtc_close = function (conn) { rtcClose(conn); };
    env.ivn_rtc_set_remote = function (conn, kind, ptr, len) { rtcSetRemote(conn, kind, readStr(ptr, len)); };
    env.ivn_rtc_add_ice = function (conn, ptr, len) { rtcAddIce(conn, readStr(ptr, len)); };

    env.ivn_rtc_poll_signal = function (out, cap) {
      if (sigOut.length === 0) return -1;
      const s = sigOut.shift();
      const body = new TextEncoder().encode(s.body);
      const total = 4 + 32 + 1 + body.length;
      if (total > cap) return -1;
      const view = mem();
      // conn slot is unused on the outbound path; leave it 0
      view[out] = 0; view[out + 1] = 0; view[out + 2] = 0; view[out + 3] = 0;
      view.set(s.peer, out + 4);
      view[out + 36] = s.kind;
      view.set(body, out + 37);
      return total;
    };

    env.ivn_rtc_poll_event = function (out, cap) {
      if (evtOut.length === 0) return -1;
      const e = evtOut.shift();
      const total = 5 + e.bytes.length;
      if (total > cap) return -1;
      const view = mem();
      view[out] = e.conn & 0xff;
      view[out + 1] = (e.conn >> 8) & 0xff;
      view[out + 2] = (e.conn >> 16) & 0xff;
      view[out + 3] = (e.conn >> 24) & 0xff;
      view[out + 4] = e.type;
      view.set(e.bytes, out + 5);
      return total;
    };
  }

  miniquad_add_plugin({
    register_plugin: register,
    on_init: function () {},
    version: "0.1.0",
    name: "ironvein_net",
  });
})();
