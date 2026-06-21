//! browser.rs — the wasm32 matchmaking orchestrator.
//!
//! Ties the three browser-only pieces together so the client's frame loop
//! only has to call two methods:
//!   * `Matchmaker::pump()` — once a frame, to move SDP/ICE between the
//!     RTCPeerConnections (JS) and the Nostr relays (JS), via Rust crypto;
//!   * `Session::update()` — the unchanged lockstep engine, now riding a
//!     `WebRtcMesh` transport.
//!
//! Hosting and joining both reuse the existing sans-io `Session`/`Joiner`:
//! WebRTC is just another `Transport`, Nostr just another way to find a peer.

use crate::crypto::{hex_encode, Identity, PubKey};
use crate::nostr::Beacon;
use crate::signaling::Lobby;
use crate::transport_webrtc::{now_ms, pump_signaling, WasmRelay, WebRtcMesh};
use crate::{Joiner, Session};
use ironvein_sim::World;

/// Owns the relay lobby and pumps the WebRTC<->Nostr signaling bridge. One
/// per browser tab, shared across browsing, hosting, and playing (the JS
/// relay/RTC state is a singleton, so a single Matchmaker drives it all).
pub struct Matchmaker {
    lobby: Lobby<WasmRelay>,
    region: String,
    last_advert: u64,
    /// when true, our beacon is invite-only (no global topic): only peers who
    /// know our room code can find us. Set by `go_private`.
    private: bool,
}

impl Matchmaker {
    /// Connect to the given relays and start listening for region beacons
    /// and signals addressed to us.
    pub fn new(identity: Identity, relays: &[&str], region: &str) -> Matchmaker {
        // Subscribe to EVERY region's beacons (empty slice -> the shared
        // `ironvein` topic), not just our own — otherwise the world map can
        // only ever see the sector we host, and cross-region travel has
        // nothing to discover. We still advertise a beacon for `region` alone.
        // `since = now - 120s` keeps relays that wrongly persist our ephemeral
        // beacons from replaying their entire backlog (the kind=29001 flood).
        let since = (now_ms() / 1000).saturating_sub(120);
        let lobby = Lobby::new(WasmRelay::connect(relays), identity, &[], Some(since));
        Matchmaker { lobby, region: region.to_string(), last_advert: 0, private: false }
    }

    /// Our stable, shareable room code (derived from our identity key). Hand it
    /// to a friend; they type it to find an invite-only colony we host.
    pub fn room_code(&self) -> String {
        crate::nostr::host_room_code(&self.my_key())
    }

    /// Switch hosting to invite-only: beacon on the code-derived private region
    /// (no global topic), so only peers who enter our `room_code()` discover us.
    pub fn go_private(&mut self) {
        self.region = crate::nostr::room_code_region(&self.room_code());
        self.private = true;
        self.last_advert = 0; // beacon promptly under the new topic
    }

    /// Joiner: start listening for the invite-only host behind `code`. After
    /// this, that host's beacon shows up in `regions()` (its region equals
    /// `room_code_region(code)`), and you dial it like any other.
    pub fn watch_code(&mut self, code: &str) {
        self.lobby.watch_region(&crate::nostr::room_code_region(code));
    }

    pub fn my_key(&self) -> PubKey {
        self.lobby.my_key()
    }

    /// Drive signaling for one frame. Call before/after `Session::update`.
    pub fn pump(&mut self) {
        pump_signaling(&mut self.lobby);
    }

    /// Host only: re-publish our region beacon about once a second, including
    /// who currently controls the region (paints the world map).
    pub fn advertise(&mut self, tick: u32, players: u32, genesis: u64, controller: PubKey, controller_name: &str) {
        // Re-publish every few seconds, not every second: public relays
        // rate-limit aggressive writers ("noting too much"), and a throttled
        // beacon writer leaves headroom for the bursty signaling events
        // (offer/answer/ICE) that a join actually depends on.
        let now = now_ms() / 1000;
        if now.saturating_sub(self.last_advert) < 8 {
            return;
        }
        self.last_advert = now;
        let beacon = Beacon {
            region: self.region.clone(),
            host: self.my_key(),
            tick,
            players,
            genesis,
            controller,
            controller_name: controller_name.to_string(),
            private: self.private,
        };
        self.lobby.advertise(&beacon, now);
    }

    /// Live regions discovered so far (freshest first).
    pub fn regions(&self) -> Vec<Beacon> {
        self.lobby.regions()
    }
}

/// Host a fresh/loaded world over WebRTC. The returned session behaves
/// exactly like the native host — joiners dial it by pubkey instead of IP.
pub fn host(world: World, identity: Identity, name: &str, color: u8, bots: &[String]) -> Session {
    Session::host_on(Box::new(WebRtcMesh::new()), world, identity, name, color, bots)
}

/// Start joining the region whose host pubkey is `host_key`. Drive the
/// returned `Joiner::poll()` each frame (while also pumping the Matchmaker)
/// until it yields the live `Session`. The host's Welcome is pinned to
/// `host_key`, so a relay can't substitute an impostor.
pub fn join(host_key: &PubKey, identity: Identity, name: &str, color: u8) -> Option<Joiner> {
    let addr = hex_encode(host_key);
    let mut joiner = Joiner::new(Box::new(WebRtcMesh::new()), &addr, identity, name, color)?;
    joiner.expect_host_key(*host_key);
    Some(joiner)
}
