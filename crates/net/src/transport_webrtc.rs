//! transport_webrtc.rs — the browser transport: a WebRTC data-channel mesh,
//! signaled over Nostr. Compiles only for `wasm32`.
//!
//! The split with JavaScript (see `js/ironvein_net.js`):
//!   * **JS owns the sockets** — the `wss://` relay connections and the
//!     `RTCPeerConnection`s / `RTCDataChannel`s. It speaks no crypto and
//!     makes no decisions; it is a dumb pipe with a poll queue.
//!   * **Rust owns the meaning** — Nostr event signing/verification, ECDH
//!     encryption of SDP/ICE, matchmaking, and the entire lockstep session.
//!
//! The FFI is poll-based to match the sans-io `Session`: JS buffers inbound
//! data and Rust drains it each frame into a caller-provided wasm buffer.
//! Nothing blocks; nothing calls back into Rust.

use crate::nostr::Signal;
use crate::signaling::{Lobby, RelayClient};
use crate::transport::{ConnId, Transport, TransportEv};
use crate::crypto::{hex_decode32, hex_encode, PubKey};

// ----------------------------------------------------------------------
// The JavaScript boundary (implemented by js/ironvein_net.js)
// ----------------------------------------------------------------------

extern "C" {
    // --- relays (Nostr over WebSocket) ---
    /// Open a relay connection. `url` is utf-8 at `ptr..ptr+len`.
    fn ivn_relay_connect(ptr: *const u8, len: usize);
    /// Send a raw NIP-01 client message (e.g. `["EVENT",{…}]`, `["REQ",…]`).
    fn ivn_relay_send(ptr: *const u8, len: usize);
    /// Copy the next inbound relay text frame into `out..out+cap`. Returns
    /// its byte length, or -1 if the queue is empty.
    fn ivn_relay_poll(out: *mut u8, cap: usize) -> i32;

    // --- WebRTC mesh ---
    /// Begin an outbound connection (we are the offerer) to the peer whose
    /// x-only pubkey hex is at `ptr..ptr+len`. Returns a connection handle.
    fn ivn_rtc_dial(ptr: *const u8, len: usize) -> u32;
    /// Begin an inbound connection (we are the answerer) for an offer that
    /// arrived from the peer at `ptr..ptr+len`. Returns a connection handle.
    fn ivn_rtc_accept(ptr: *const u8, len: usize) -> u32;
    /// Send a data-channel frame on `conn`.
    fn ivn_rtc_send(conn: u32, ptr: *const u8, len: usize);
    /// Tear down `conn`.
    fn ivn_rtc_close(conn: u32);
    /// Feed a remote description to `conn`. `kind`: 0 = offer, 1 = answer.
    fn ivn_rtc_set_remote(conn: u32, kind: u32, ptr: *const u8, len: usize);
    /// Add a remote ICE candidate to `conn`.
    fn ivn_rtc_add_ice(conn: u32, ptr: *const u8, len: usize);
    /// Drain one outbound signal this PC wants sent over Nostr. Layout at
    /// `out`: `conn(u32 le) | peer_pubkey(32) | kind(u8) | body…`, where
    /// kind 0=offer, 1=answer, 2=ice. Returns total length, or -1 if none.
    fn ivn_rtc_poll_signal(out: *mut u8, cap: usize) -> i32;
    /// Drain one mesh event. Layout at `out`: `conn(u32 le) | type(u8) |
    /// bytes…`, type 0=connected, 1=data, 2=closed. Returns length or -1.
    fn ivn_rtc_poll_event(out: *mut u8, cap: usize) -> i32;
    /// Milliseconds since the Unix epoch (for `created_at` / watchdogs).
    fn ivn_now_ms() -> f64;
}

const BUF: usize = 64 * 1024;

// ----------------------------------------------------------------------
// WasmRelay — a RelayClient backed by the JS WebSocket pool
// ----------------------------------------------------------------------

pub struct WasmRelay;

impl WasmRelay {
    /// Connect to a set of relay URLs (e.g. ["wss://relay.damus.io"]).
    pub fn connect(urls: &[&str]) -> WasmRelay {
        for url in urls {
            unsafe { ivn_relay_connect(url.as_ptr(), url.len()) };
        }
        WasmRelay
    }
}

impl RelayClient for WasmRelay {
    fn publish(&mut self, ev: &crate::nostr::Event) {
        let frame = format!("[\"EVENT\",{}]", ev.to_json());
        unsafe { ivn_relay_send(frame.as_ptr(), frame.len()) };
    }
    fn subscribe(&mut self, filter: crate::signaling::Filter) {
        // A NIP-01 REQ that reuses a subscription id REPLACES the previous one
        // on real relays. The lobby installs two filters (beacons, signals);
        // if both used id "ivn" the second would clobber the first, and the
        // peer would go deaf to either discovery or signaling. So derive a
        // distinct id per filter from its kinds. (MockRelay is additive, which
        // is why the native test suite never surfaced this.)
        let mut sub = String::from("ivn");
        for k in &filter.kinds {
            sub.push('-');
            sub.push_str(&k.to_string());
        }
        let frame = format!("[\"REQ\",\"{}\",{}]", sub, filter.to_json());
        unsafe { ivn_relay_send(frame.as_ptr(), frame.len()) };
    }
    fn poll(&mut self) -> Vec<crate::nostr::Event> {
        let mut buf = vec![0u8; BUF];
        let mut out = Vec::new();
        loop {
            let n = unsafe { ivn_relay_poll(buf.as_mut_ptr(), buf.len()) };
            if n < 0 {
                break;
            }
            let text = match std::str::from_utf8(&buf[..n as usize]) {
                Ok(t) => t,
                Err(_) => continue,
            };
            // relay -> client: ["EVENT",<sub>,<event>]
            if let Some(json) = crate::nostr::json::parse(text) {
                if let Some(arr) = json.as_arr() {
                    if arr.first().and_then(|v| v.as_str()) == Some("EVENT") {
                        if let Some(ev) = arr.get(2).and_then(crate::nostr::Event::from_json) {
                            out.push(ev);
                        }
                    }
                }
            }
        }
        out
    }
}

pub fn now_ms() -> u64 {
    unsafe { ivn_now_ms() as u64 }
}

// ----------------------------------------------------------------------
// WebRtcMesh — the Transport
// ----------------------------------------------------------------------

pub struct WebRtcMesh;

impl WebRtcMesh {
    pub fn new() -> WebRtcMesh {
        WebRtcMesh
    }
}

impl Default for WebRtcMesh {
    fn default() -> Self {
        WebRtcMesh::new()
    }
}

impl Transport for WebRtcMesh {
    fn poll(&mut self) -> Vec<TransportEv> {
        let mut buf = vec![0u8; BUF];
        let mut out = Vec::new();
        loop {
            let n = unsafe { ivn_rtc_poll_event(buf.as_mut_ptr(), buf.len()) };
            if n < 0 {
                break;
            }
            let n = n as usize;
            if n < 5 {
                continue;
            }
            let conn = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as ConnId;
            match buf[4] {
                0 => out.push(TransportEv::Connected { conn }),
                1 => out.push(TransportEv::Data { conn, bytes: buf[5..n].to_vec() }),
                2 => out.push(TransportEv::Closed { conn }),
                _ => {}
            }
        }
        out
    }

    fn send(&mut self, conn: ConnId, bytes: &[u8]) {
        unsafe { ivn_rtc_send(conn as u32, bytes.as_ptr(), bytes.len()) };
    }

    fn dial(&mut self, addr: &str) -> Option<ConnId> {
        // addr is the host's x-only pubkey hex
        Some(unsafe { ivn_rtc_dial(addr.as_ptr(), addr.len()) } as ConnId)
    }

    fn close(&mut self, conn: ConnId) {
        unsafe { ivn_rtc_close(conn as u32) };
    }

    fn remote_ip(&self, _conn: ConnId) -> String {
        String::new() // WebRTC peers are dialed by pubkey, not address
    }

    fn listen_port(&self) -> u16 {
        0
    }

    fn now_s(&self) -> f64 {
        now_ms() as f64 / 1000.0
    }
}

// ----------------------------------------------------------------------
// The signaling bridge — shuttle SDP/ICE between JS-RTC and Nostr
// ----------------------------------------------------------------------

/// Pump one frame's worth of signaling: deliver inbound Nostr signals to the
/// right peer connections, and publish outbound ones the PCs produced. Call
/// every frame alongside `Session::update`. Returns the host pubkeys for
/// which a *new* inbound offer was just accepted (so callers can note them).
pub fn pump_signaling<R: RelayClient>(lobby: &mut Lobby<R>) -> Vec<PubKey> {
    let now = now_ms() / 1000;
    let mut accepted = Vec::new();

    // inbound: Nostr -> RTC
    for (from, sig) in lobby.poll() {
        let hexk = hex_encode(&from);
        match sig {
            Signal::Offer(sdp) => {
                // an unknown peer is dialing us: stand up an answerer PC and
                // feed it the offer
                let conn = unsafe { ivn_rtc_accept(hexk.as_ptr(), hexk.len()) };
                unsafe { ivn_rtc_set_remote(conn, 0, sdp.as_ptr(), sdp.len()) };
                accepted.push(from);
            }
            Signal::Answer(sdp) => {
                // our outbound PC for this peer applies the answer; JS maps
                // peer pubkey -> conn internally
                let conn = unsafe { ivn_rtc_accept(hexk.as_ptr(), hexk.len()) };
                unsafe { ivn_rtc_set_remote(conn, 1, sdp.as_ptr(), sdp.len()) };
            }
            Signal::Ice(cand) => {
                let conn = unsafe { ivn_rtc_accept(hexk.as_ptr(), hexk.len()) };
                unsafe { ivn_rtc_add_ice(conn, cand.as_ptr(), cand.len()) };
            }
        }
    }

    // outbound: RTC -> Nostr
    let mut buf = vec![0u8; BUF];
    loop {
        let n = unsafe { ivn_rtc_poll_signal(buf.as_mut_ptr(), buf.len()) };
        if n < 0 {
            break;
        }
        let n = n as usize;
        if n < 4 + 32 + 1 {
            continue;
        }
        let mut peer = [0u8; 32];
        peer.copy_from_slice(&buf[4..36]);
        let kind = buf[36];
        let body = String::from_utf8_lossy(&buf[37..n]).into_owned();
        let sig = match kind {
            0 => Signal::Offer(body),
            1 => Signal::Answer(body),
            2 => Signal::Ice(body),
            _ => continue,
        };
        lobby.send_signal(&peer, &sig, now);
    }

    accepted
}

/// Convenience: decode an x-only pubkey from its hex (for region dialing).
pub fn key_from_hex(s: &str) -> Option<PubKey> {
    hex_decode32(s)
}
