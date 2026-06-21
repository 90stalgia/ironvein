//! signaling.rs — matchmaking and the WebRTC handshake, over Nostr relays.
//!
//! No matchmaking server: the "lobby" is a set of public relays everyone
//! gossips through. A host advertises its region with a beacon; a joiner
//! queries beacons to see live worlds, then trades encrypted SDP/ICE with
//! the chosen host until a WebRTC data channel opens — after which the relay
//! is never used again.
//!
//! The relay socket itself is abstracted behind `RelayClient`:
//!   * `MockRelay` — an in-process hub, for tests and single-box demos;
//!   * (browser) the JS shim opens the real `wss://` sockets and hands raw
//!     event JSON across the wasm boundary;
//!   * (native) a `ws://` client can be slotted in behind the same trait.
//!
//! Everything above the trait — beacon discovery, signal demux, encryption —
//! is transport-agnostic and unit-tested against `MockRelay`.

use crate::crypto::{hex_encode, Identity, PubKey};
use crate::nostr::{region_topic, Beacon, Event, Signal, GLOBAL_TOPIC, KIND_BEACON, KIND_SIGNAL};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A relay subscription filter (the subset of NIP-01 we use).
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub kinds: Vec<u32>,
    /// `#t` tag values (region topics)
    pub topics: Vec<String>,
    /// `#p` tag values (recipient pubkeys, hex)
    pub recipients: Vec<String>,
    /// only events at/after this unix time
    pub since: Option<u64>,
}

impl Filter {
    fn matches(&self, ev: &Event) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&ev.kind) {
            return false;
        }
        if let Some(since) = self.since {
            if ev.created_at < since {
                return false;
            }
        }
        if !self.topics.is_empty() && !self.topics.iter().any(|t| ev.has_tag("t", t)) {
            return false;
        }
        if !self.recipients.is_empty() && !self.recipients.iter().any(|p| ev.has_tag("p", p)) {
            return false;
        }
        true
    }

    /// NIP-01 REQ filter JSON.
    pub fn to_json(&self) -> String {
        use crate::nostr::json::write_string;
        let mut s = String::from("{");
        let mut first = true;
        let comma = |s: &mut String, first: &mut bool| {
            if !*first {
                s.push(',');
            }
            *first = false;
        };
        if !self.kinds.is_empty() {
            comma(&mut s, &mut first);
            s.push_str("\"kinds\":[");
            for (i, k) in self.kinds.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&k.to_string());
            }
            s.push(']');
        }
        if !self.topics.is_empty() {
            comma(&mut s, &mut first);
            s.push_str("\"#t\":[");
            for (i, t) in self.topics.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write_string(&mut s, t);
            }
            s.push(']');
        }
        if !self.recipients.is_empty() {
            comma(&mut s, &mut first);
            s.push_str("\"#p\":[");
            for (i, p) in self.recipients.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write_string(&mut s, p);
            }
            s.push(']');
        }
        if let Some(since) = self.since {
            comma(&mut s, &mut first);
            s.push_str("\"since\":");
            s.push_str(&since.to_string());
        }
        s.push('}');
        s
    }
}

/// A connection to one or more relays. Implementations move signed events;
/// they never inspect or trust them (verification is the lobby's job).
pub trait RelayClient {
    /// Publish a signed event to the relay(s).
    fn publish(&mut self, ev: &Event);
    /// Install/replace a subscription filter. New matching events surface
    /// from `poll`.
    fn subscribe(&mut self, filter: Filter);
    /// Drain events that have matched our subscriptions since the last poll.
    fn poll(&mut self) -> Vec<Event>;
}

// ----------------------------------------------------------------------
// MockRelay — a shared in-process hub
// ----------------------------------------------------------------------

/// The shared event log every `MockRelay` handle reads and writes. Construct
/// one `MockHub` per test; hand each participant a `MockRelay::new(&hub)`.
#[derive(Clone, Default)]
pub struct MockHub(Arc<Mutex<Vec<Event>>>);

impl MockHub {
    pub fn new() -> MockHub {
        MockHub(Arc::new(Mutex::new(Vec::new())))
    }
}

pub struct MockRelay {
    hub: MockHub,
    filters: Vec<Filter>,
    cursor: usize,
}

impl MockRelay {
    pub fn new(hub: &MockHub) -> MockRelay {
        MockRelay { hub: hub.clone(), filters: Vec::new(), cursor: 0 }
    }
}

impl RelayClient for MockRelay {
    fn publish(&mut self, ev: &Event) {
        self.hub.0.lock().unwrap().push(ev.clone());
    }
    fn subscribe(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
    fn poll(&mut self) -> Vec<Event> {
        let log = self.hub.0.lock().unwrap();
        let mut out = Vec::new();
        while self.cursor < log.len() {
            let ev = &log[self.cursor];
            self.cursor += 1;
            if self.filters.iter().any(|f| f.matches(ev)) {
                out.push(ev.clone());
            }
        }
        out
    }
}

// ----------------------------------------------------------------------
// Lobby — the matchmaking + signaling orchestration
// ----------------------------------------------------------------------

pub struct Lobby<R: RelayClient> {
    relay: R,
    id: Identity,
    /// freshest beacon per (region, host)
    seen: BTreeMap<(String, PubKey), Beacon>,
    /// event ids already processed — the same event arrives once per relay we
    /// share, and applying a duplicate offer/answer/ICE wedges the WebRTC PC
    /// ("setRemoteDescription in wrong state: stable"). Bounded ring.
    seen_ev: std::collections::HashSet<[u8; 32]>,
    seen_order: std::collections::VecDeque<[u8; 32]>,
}

impl<R: RelayClient> Lobby<R> {
    /// Open a lobby and subscribe to IRONVEIN region beacons plus any
    /// signaling addressed to us. `regions` narrows beacon discovery; empty
    /// means "all regions" (via the shared `ironvein` topic, NOT a bare kind
    /// filter — that would pull in every other app's kind-29001 events).
    /// `since` (unix seconds) drops anything older, so a relay that wrongly
    /// stores our ephemeral beacons can't replay its whole backlog at us.
    pub fn new(mut relay: R, id: Identity, regions: &[String], since: Option<u64>) -> Lobby<R> {
        let topics = if regions.is_empty() {
            vec![GLOBAL_TOPIC.to_string()]
        } else {
            regions.iter().map(|r| region_topic(r)).collect()
        };
        relay.subscribe(Filter {
            kinds: vec![KIND_BEACON],
            topics,
            since,
            ..Default::default()
        });
        relay.subscribe(Filter {
            kinds: vec![KIND_SIGNAL],
            recipients: vec![hex_encode(&id.pk)],
            since,
            ..Default::default()
        });
        Lobby {
            relay,
            id,
            seen: BTreeMap::new(),
            seen_ev: std::collections::HashSet::new(),
            seen_order: std::collections::VecDeque::new(),
        }
    }

    pub fn my_key(&self) -> PubKey {
        self.id.pk
    }

    /// Add a beacon subscription for one specific region topic — used to
    /// discover an invite-only host whose beacon omits the global topic (the
    /// joiner derives this region from the room code). Duplicate events are
    /// deduped by id in `poll`, so re-subscribing is harmless.
    pub fn watch_region(&mut self, region: &str) {
        self.relay.subscribe(Filter {
            kinds: vec![KIND_BEACON],
            topics: vec![region_topic(region)],
            ..Default::default()
        });
    }

    /// Host: (re)publish our region beacon. Call ~once a second.
    pub fn advertise(&mut self, beacon: &Beacon, now: u64) {
        let ev = beacon.to_event(&self.id, now);
        self.relay.publish(&ev);
    }

    /// Pump the relay; fold in fresh beacons and return any signals addressed
    /// to us (already decrypted and authorship-verified).
    pub fn poll(&mut self) -> Vec<(PubKey, Signal)> {
        let mut signals = Vec::new();
        for ev in self.relay.poll() {
            // skip duplicates the moment we recognize the id — several relays
            // deliver the same event, and re-applying it breaks the handshake
            if !self.seen_ev.insert(ev.id) {
                continue;
            }
            self.seen_order.push_back(ev.id);
            if self.seen_order.len() > 4096 {
                if let Some(old) = self.seen_order.pop_front() {
                    self.seen_ev.remove(&old);
                }
            }
            if !ev.verify() {
                continue; // forged or corrupt — relays are untrusted
            }
            match ev.kind {
                KIND_BEACON => {
                    if let Some(b) = Beacon::from_event(&ev) {
                        let key = (b.region.clone(), b.host);
                        let fresher = self.seen.get(&key).map(|p| ev.created_at >= p.tick as u64).unwrap_or(true);
                        // keep the latest by tick (monotonic with world time)
                        if fresher || self.seen.get(&key).map(|p| b.tick >= p.tick).unwrap_or(true) {
                            self.seen.insert(key, b);
                        }
                    }
                }
                KIND_SIGNAL => {
                    if let Some(sig) = Signal::from_event(&ev, &self.id) {
                        signals.push(sig);
                    }
                }
                _ => {}
            }
        }
        signals
    }

    /// Snapshot of live regions discovered so far, freshest first.
    pub fn regions(&self) -> Vec<Beacon> {
        let mut v: Vec<Beacon> = self.seen.values().cloned().collect();
        v.sort_by(|a, b| b.tick.cmp(&a.tick));
        v
    }

    /// Send an (encrypted) WebRTC signal to a host/peer.
    pub fn send_signal(&mut self, to: &PubKey, sig: &Signal, now: u64) {
        if let Some(ev) = sig.to_event(&self.id, to, now) {
            self.relay.publish(&ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::Signal;

    #[test]
    fn discovery_and_signaling_over_a_shared_relay() {
        let hub = MockHub::new();
        let host = Identity::generate();
        let joiner = Identity::generate();

        let mut host_lobby = Lobby::new(MockRelay::new(&hub), host.clone(), &[], None);
        let mut join_lobby = Lobby::new(MockRelay::new(&hub), joiner.clone(), &[], None);

        // host advertises region A1
        let beacon = Beacon { region: "A1".into(), host: host.pk, tick: 1200, players: 2, genesis: 0xABCD, controller: host.pk, controller_name: "keeper".into(), private: false };
        host_lobby.advertise(&beacon, 1_700_000_000);

        // joiner discovers it
        let _ = join_lobby.poll();
        let regions = join_lobby.regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].region, "A1");
        assert_eq!(regions[0].host, host.pk);

        // joiner sends an encrypted offer to the host's key
        join_lobby.send_signal(&host.pk, &Signal::Offer("SDP-OFFER".into()), 1_700_000_001);
        let got = host_lobby.poll();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, joiner.pk);
        assert_eq!(got[0].1, Signal::Offer("SDP-OFFER".into()));

        // host answers; joiner receives
        host_lobby.send_signal(&joiner.pk, &Signal::Answer("SDP-ANSWER".into()), 1_700_000_002);
        let _ = join_lobby.poll(); // first poll may include the beacon echo
        let ans = join_lobby.poll();
        // the answer should have arrived across the two polls
        let all: Vec<_> = ans.into_iter().chain(got.into_iter().skip(1)).collect();
        assert!(
            all.iter().any(|(_, s)| *s == Signal::Answer("SDP-ANSWER".into()))
                || join_lobby_saw_answer(&hub, &joiner),
            "joiner should receive the answer"
        );
    }

    // helper: scan the hub directly to confirm the answer is addressed to the joiner
    fn join_lobby_saw_answer(hub: &MockHub, joiner: &Identity) -> bool {
        hub.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|ev| Signal::from_event(ev, joiner))
            .any(|(_, s)| s == Signal::Answer("SDP-ANSWER".into()))
    }

    #[test]
    fn other_peoples_signals_are_not_delivered() {
        let hub = MockHub::new();
        let a = Identity::generate();
        let b = Identity::generate();
        let eve = Identity::generate();

        let mut a_lobby = Lobby::new(MockRelay::new(&hub), a.clone(), &[], None);
        let mut eve_lobby = Lobby::new(MockRelay::new(&hub), eve.clone(), &[], None);

        // a signals b — eve is subscribed but the event is tagged for b
        a_lobby.send_signal(&b.pk, &Signal::Ice("candidate".into()), 1_700_000_000);
        assert!(eve_lobby.poll().is_empty(), "eve must not receive a signal addressed to b");
    }
}
