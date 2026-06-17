//! transport.rs — the seam between lockstep logic and the actual network.
//!
//! `Session` never touches a socket. It consumes `TransportEv`s from and
//! emits frames through this trait, so the same lockstep engine runs over
//! a TCP mesh (native, reader threads), WebRTC data channels (wasm32,
//! single-threaded, polled from the frame loop), or nothing at all (solo
//! worlds, deterministic tests).
//!
//! Identification is protocol business, not transport business: a fresh
//! link is anonymous until its first (signed) frame tells the session who
//! it is. Transports move opaque bytes; envelopes, signatures and message
//! decoding all live above this seam.

/// Opaque handle to one peer link. Never reused within a session.
pub type ConnId = u64;

#[derive(Debug)]
pub enum TransportEv {
    /// A link (inbound or dialed) is up and can carry frames.
    Connected { conn: ConnId },
    /// A raw frame (an encoded, signed Envelope) arrived on `conn`.
    Data { conn: ConnId, bytes: Vec<u8> },
    /// The link is gone (error, close, or remote shutdown).
    Closed { conn: ConnId },
}

pub trait Transport: Send {
    /// Drain pending network events. Non-blocking; call once per frame.
    fn poll(&mut self) -> Vec<TransportEv>;
    /// Send a raw frame. Sends on a still-dialing conn are queued and
    /// flushed when it connects; sends on a dead conn drop silently.
    fn send(&mut self, conn: ConnId, bytes: &[u8]);
    /// Start connecting to a peer address ("ip:port" for TCP, hex pubkey
    /// for WebRTC). Returns a ConnId immediately; `Connected` (or
    /// `Closed`, on failure) follows from `poll`.
    fn dial(&mut self, addr: &str) -> Option<ConnId>;
    /// Tear a link down. No `Closed` event is emitted for our own closes.
    fn close(&mut self, conn: ConnId);
    /// Remote IP of a conn (TCP only), for building dial-back addresses.
    fn remote_ip(&self, conn: ConnId) -> String;
    /// Local port peers can dial (TCP only). 0 where meaningless.
    fn listen_port(&self) -> u16;
    /// Wall-clock seconds since an arbitrary epoch, for liveness watchdogs.
    /// (A platform service, so it lives on the transport seam — the sim
    /// clock is paced `dt`, which tests run faster than real time, and
    /// `std::time::Instant` does not exist on wasm32.) The default 0.0
    /// disables watchdogs entirely.
    fn now_s(&self) -> f64 {
        0.0
    }
}

/// Solo worlds and unit tests: no peers, no events, sends vanish.
pub struct NullTransport;

impl Transport for NullTransport {
    fn poll(&mut self) -> Vec<TransportEv> {
        Vec::new()
    }
    fn send(&mut self, _: ConnId, _: &[u8]) {}
    fn dial(&mut self, _: &str) -> Option<ConnId> {
        None
    }
    fn close(&mut self, _: ConnId) {}
    fn remote_ip(&self, _: ConnId) -> String {
        String::new()
    }
    fn listen_port(&self) -> u16 {
        0
    }
}
