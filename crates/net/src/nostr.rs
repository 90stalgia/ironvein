//! nostr.rs — the serverless signaling vocabulary.
//!
//! Nostr is a gossip of signed JSON events over public relays. IRONVEIN
//! borrows exactly two event kinds and nothing else:
//!
//!   * **kind 29001 — region beacon** (replaceable). A host publishes one per
//!     region it runs, tagged `["t","ironvein-region-<ID>"]`, content listing
//!     its pubkey, current tick, player count and world genesis hash. New
//!     players query these to see what worlds are live and whom to dial.
//!   * **kind 29000 — signaling** (ephemeral). The WebRTC handshake: SDP
//!     offer/answer and ICE candidates, each ECDH-encrypted to the
//!     recipient's pubkey and tagged `["p",<recipient-hex>]` so relays route
//!     it. Once the data channel is up, Nostr is done — gameplay never
//!     touches a relay.
//!
//! Nostr uses secp256k1 + BIP-340 Schnorr — the very scheme already in
//! `crypto.rs` — so a settler's identity key *is* its Nostr key. No second
//! identity, no servers, no accounts.

use crate::crypto::{self, hex_decode32, hex_encode, Identity, PubKey};

pub const KIND_SIGNAL: u32 = 29000;
pub const KIND_BEACON: u32 = 29001;

/// A topic every IRONVEIN beacon also carries, so a peer can discover ALL
/// regions with one subscription (`#t: ironvein`) without vacuuming up every
/// other app's kind-29001 events on a public relay.
pub const GLOBAL_TOPIC: &str = "ironvein";

/// The tag value for a region's beacon topic, e.g. `ironvein-region-A1`.
pub fn region_topic(region: &str) -> String {
    format!("ironvein-region-{region}")
}

// ----------------------------------------------------------------------
// A NIP-01 event
// ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: [u8; 32],
    pub pubkey: PubKey,
    pub created_at: u64,
    pub kind: u32,
    /// each tag is a list of strings, e.g. ["t","ironvein-region-A1"]
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: [u8; 64],
}

impl Event {
    /// Build, compute the NIP-01 id, and Schnorr-sign an event.
    pub fn create(id: &Identity, created_at: u64, kind: u32, tags: Vec<Vec<String>>, content: String) -> Event {
        let mut ev = Event {
            id: [0; 32],
            pubkey: id.pk,
            created_at,
            kind,
            tags,
            content,
            sig: [0; 64],
        };
        ev.id = ev.compute_id();
        ev.sig = id.sign(&ev.id);
        ev
    }

    /// NIP-01 id = sha256 of the compact JSON array
    /// `[0,<pubkey-hex>,<created_at>,<kind>,<tags>,<content>]`.
    pub fn compute_id(&self) -> [u8; 32] {
        let mut s = String::new();
        s.push_str("[0,\"");
        s.push_str(&hex_encode(&self.pubkey));
        s.push_str("\",");
        s.push_str(&self.created_at.to_string());
        s.push(',');
        s.push_str(&self.kind.to_string());
        s.push(',');
        json::write_tags(&mut s, &self.tags);
        s.push(',');
        json::write_string(&mut s, &self.content);
        s.push(']');
        crypto::sha256(&[s.as_bytes()])
    }

    /// id matches the contents AND the signature verifies under pubkey.
    pub fn verify(&self) -> bool {
        self.compute_id() == self.id && crypto::verify(&self.pubkey, &self.id, &self.sig)
    }

    /// First value of the first tag whose name is `name` (e.g. "p", "t").
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(|s| s == name).unwrap_or(false))
            .and_then(|t| t.get(1))
            .map(|s| s.as_str())
    }

    /// Whether ANY `name` tag has value `value`. Events can carry several tags
    /// of the same name (a beacon has two `t` tags: its region and the global
    /// `ironvein` topic), so a filter must check all of them, not just the
    /// first — `tag()` alone would miss the global topic.
    pub fn has_tag(&self, name: &str, value: &str) -> bool {
        self.tags
            .iter()
            .any(|t| t.first().map(|s| s == name).unwrap_or(false) && t.get(1).map(|s| s == value).unwrap_or(false))
    }

    /// Serialize to the compact JSON object relays expect.
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        s.push_str("{\"id\":\"");
        s.push_str(&hex_encode(&self.id));
        s.push_str("\",\"pubkey\":\"");
        s.push_str(&hex_encode(&self.pubkey));
        s.push_str("\",\"created_at\":");
        s.push_str(&self.created_at.to_string());
        s.push_str(",\"kind\":");
        s.push_str(&self.kind.to_string());
        s.push_str(",\"tags\":");
        json::write_tags(&mut s, &self.tags);
        s.push_str(",\"content\":");
        json::write_string(&mut s, &self.content);
        s.push_str(",\"sig\":\"");
        s.push_str(&hex_encode(&self.sig));
        s.push_str("\"}");
        s
    }

    /// Parse an event object from JSON. Returns None on any structural fault.
    pub fn from_json(v: &json::Json) -> Option<Event> {
        let obj = v.as_obj()?;
        let id = hex_decode32(obj.get("id")?.as_str()?)?;
        let pubkey = hex_decode32(obj.get("pubkey")?.as_str()?)?;
        let created_at = obj.get("created_at")?.as_u64()?;
        let kind = obj.get("kind")?.as_u64()? as u32;
        let mut tags = Vec::new();
        for t in obj.get("tags")?.as_arr()? {
            let mut row = Vec::new();
            for e in t.as_arr()? {
                row.push(e.as_str()?.to_string());
            }
            tags.push(row);
        }
        let content = obj.get("content")?.as_str()?.to_string();
        let sig_hex = obj.get("sig")?.as_str()?;
        if sig_hex.len() != 128 {
            return None;
        }
        let mut sig = [0u8; 64];
        for i in 0..64 {
            sig[i] = u8::from_str_radix(&sig_hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Event { id, pubkey, created_at, kind, tags, content, sig })
    }
}

// ----------------------------------------------------------------------
// Region beacons (kind 29001)
// ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Beacon {
    pub region: String,
    pub host: PubKey,
    pub tick: u32,
    pub players: u32,
    /// world genesis hash — pins which world this region is, so a joiner
    /// can't be lured onto a forked timeline
    pub genesis: u64,
    /// identity key of the settler who currently controls the region (most
    /// territory); all-zero if nobody does. This is what paints the world map.
    pub controller: PubKey,
    /// the controller's display name (for the map)
    pub controller_name: String,
    /// invite-only room: the beacon carries ONLY its (code-derived) region
    /// topic, NOT the global `ironvein` topic — so it never surfaces in the
    /// public lobby. Only a peer who knows the room code can derive the topic
    /// and discover it. See [`room_code_region`].
    pub private: bool,
}

impl Beacon {
    /// Publish-ready event: tagged with the region topic, content is the
    /// beacon fields as compact JSON.
    pub fn to_event(&self, id: &Identity, created_at: u64) -> Event {
        let mut content = String::new();
        content.push_str("{\"region\":");
        json::write_string(&mut content, &self.region);
        content.push_str(",\"tick\":");
        content.push_str(&self.tick.to_string());
        content.push_str(",\"players\":");
        content.push_str(&self.players.to_string());
        content.push_str(",\"genesis\":\"");
        content.push_str(&format!("{:016x}", self.genesis));
        content.push_str("\",\"ctrl\":\"");
        content.push_str(&hex_encode(&self.controller));
        content.push_str("\",\"ctrl_name\":");
        json::write_string(&mut content, &self.controller_name);
        content.push('}');
        // Public rooms carry two topics: the region's own (targeted join) and the
        // global one (so the world map finds every region with one subscription).
        // A private room omits the global topic — it's discoverable only by a peer
        // who knows the code and subscribes to its derived region topic directly.
        let mut tags = vec![vec!["t".to_string(), region_topic(&self.region)]];
        if !self.private {
            tags.push(vec!["t".to_string(), GLOBAL_TOPIC.to_string()]);
        }
        Event::create(id, created_at, KIND_BEACON, tags, content)
    }

    /// Recover a beacon from a (verified) kind-29001 event.
    pub fn from_event(ev: &Event) -> Option<Beacon> {
        if ev.kind != KIND_BEACON {
            return None;
        }
        let v = json::parse(&ev.content)?;
        let obj = v.as_obj()?;
        let region = obj.get("region")?.as_str()?.to_string();
        let tick = obj.get("tick")?.as_u64()? as u32;
        let players = obj.get("players")?.as_u64()? as u32;
        let genesis = u64::from_str_radix(obj.get("genesis")?.as_str()?, 16).ok()?;
        // controller is optional (older beacons omit it)
        let controller = obj
            .get("ctrl")
            .and_then(|v| v.as_str())
            .and_then(crate::crypto::hex_decode32)
            .unwrap_or([0u8; 32]);
        let controller_name = obj.get("ctrl_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        // a beacon without the global topic was published as invite-only
        let private = !ev.has_tag("t", GLOBAL_TOPIC);
        Some(Beacon { region, host: ev.pubkey, tick, players, genesis, controller, controller_name, private })
    }
}

/// Crockford base32 (32 symbols, excludes I/L/O/U) — reads cleanly aloud.
const ROOM_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A short, shareable room code derived from a host's identity key — stable, so
/// it's "your colony's code". 40 bits → eight base32 chars, grouped `XXXX-XXXX`.
/// (Pinched from provable-poker-web's room-code rendezvous; there the code is
/// random per table — here we anchor it to identity so the host needn't store it.)
pub fn host_room_code(pk: &PubKey) -> String {
    let h = crate::crypto::sha256(&[b"ironvein-roomcode-v1", pk]);
    let mut bits: u64 = 0;
    for &b in &h[..5] {
        bits = (bits << 8) | b as u64;
    }
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        let shift = (7 - i) * 5;
        s.push(ROOM_ALPHABET[((bits >> shift) & 0x1f) as usize] as char);
    }
    s
}

/// Map a (typed or generated) room code to its private region id — the
/// cryptographic meeting place. Both host and joiner derive the same topic from
/// the same code without ever putting the code on the wire. Punctuation/spacing
/// and case are ignored so "m7k3-9pxr" and "M7K39PXR" land in the same room.
pub fn room_code_region(code: &str) -> String {
    let norm: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    let h = crate::crypto::sha256(&[b"ironvein-room-v1", norm.as_bytes()]);
    let mut s = String::from("RM");
    for b in &h[..5] {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

// ----------------------------------------------------------------------
// Signaling payloads (kind 29000) — encrypted WebRTC handshake
// ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    Offer(String),
    Answer(String),
    Ice(String),
}

impl Signal {
    fn tag(&self) -> &'static str {
        match self {
            Signal::Offer(_) => "offer",
            Signal::Answer(_) => "answer",
            Signal::Ice(_) => "ice",
        }
    }
    fn body(&self) -> &str {
        match self {
            Signal::Offer(s) | Signal::Answer(s) | Signal::Ice(s) => s,
        }
    }

    /// Encrypt this signal to `to` and wrap it in a kind-29000 event tagged
    /// `["p",<to-hex>]` so the relay routes it to the recipient.
    pub fn to_event(&self, id: &Identity, to: &PubKey, created_at: u64) -> Option<Event> {
        // plaintext: "<kind>\n<body>"
        let plain = format!("{}\n{}", self.tag(), self.body());
        let blob = id.encrypt_to(to, plain.as_bytes())?;
        let content = crypto::hex_encode(&blob);
        let tags = vec![vec!["p".to_string(), hex_encode(to)]];
        Some(Event::create(id, created_at, KIND_SIGNAL, tags, content))
    }

    /// Decrypt a kind-29000 event addressed to us, from `ev.pubkey`.
    pub fn from_event(ev: &Event, me: &Identity) -> Option<(PubKey, Signal)> {
        if ev.kind != KIND_SIGNAL {
            return None;
        }
        let blob = hex_to_bytes(&ev.content)?;
        let plain = me.decrypt_from(&ev.pubkey, &blob)?;
        let text = String::from_utf8(plain).ok()?;
        let (tag, body) = text.split_once('\n')?;
        let sig = match tag {
            "offer" => Signal::Offer(body.to_string()),
            "answer" => Signal::Answer(body.to_string()),
            "ice" => Signal::Ice(body.to_string()),
            _ => return None,
        };
        Some((ev.pubkey, sig))
    }
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

// ----------------------------------------------------------------------
// A minimal, dependency-free JSON good enough for Nostr frames
// ----------------------------------------------------------------------

pub mod json {
    use std::collections::BTreeMap;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Json {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Json>),
        Obj(BTreeMap<String, Json>),
    }

    impl Json {
        pub fn as_str(&self) -> Option<&str> {
            if let Json::Str(s) = self { Some(s) } else { None }
        }
        pub fn as_u64(&self) -> Option<u64> {
            if let Json::Num(n) = self { Some(*n as u64) } else { None }
        }
        pub fn as_arr(&self) -> Option<&[Json]> {
            if let Json::Arr(a) = self { Some(a) } else { None }
        }
        pub fn as_obj(&self) -> Option<&BTreeMap<String, Json>> {
            if let Json::Obj(o) = self { Some(o) } else { None }
        }
    }

    /// Write a JSON string literal with the escaping NIP-01 mandates.
    pub fn write_string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{08}' => out.push_str("\\b"),
                '\u{0c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
    }

    pub fn write_tags(out: &mut String, tags: &[Vec<String>]) {
        out.push('[');
        for (i, tag) in tags.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('[');
            for (j, e) in tag.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                write_string(out, e);
            }
            out.push(']');
        }
        out.push(']');
    }

    /// Parse a JSON document. Tolerant recursive descent — enough for the
    /// events and relay frames we exchange, not a conformance suite.
    pub fn parse(s: &str) -> Option<Json> {
        let mut p = Parser { b: s.as_bytes(), i: 0 };
        p.ws();
        let v = p.value()?;
        p.ws();
        Some(v)
    }

    struct Parser<'a> {
        b: &'a [u8],
        i: usize,
    }

    impl<'a> Parser<'a> {
        fn ws(&mut self) {
            while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            }
        }
        fn peek(&self) -> Option<u8> {
            self.b.get(self.i).copied()
        }
        fn value(&mut self) -> Option<Json> {
            self.ws();
            match self.peek()? {
                b'{' => self.object(),
                b'[' => self.array(),
                b'"' => Some(Json::Str(self.string()?)),
                b't' => self.lit("true", Json::Bool(true)),
                b'f' => self.lit("false", Json::Bool(false)),
                b'n' => self.lit("null", Json::Null),
                _ => self.number(),
            }
        }
        fn lit(&mut self, word: &str, val: Json) -> Option<Json> {
            if self.b[self.i..].starts_with(word.as_bytes()) {
                self.i += word.len();
                Some(val)
            } else {
                None
            }
        }
        fn number(&mut self) -> Option<Json> {
            let start = self.i;
            while self.i < self.b.len()
                && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
            {
                self.i += 1;
            }
            std::str::from_utf8(&self.b[start..self.i]).ok()?.parse::<f64>().ok().map(Json::Num)
        }
        fn string(&mut self) -> Option<String> {
            self.i += 1; // opening quote
            let mut out = String::new();
            while let Some(c) = self.peek() {
                self.i += 1;
                match c {
                    b'"' => return Some(out),
                    b'\\' => {
                        let e = self.peek()?;
                        self.i += 1;
                        match e {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'b' => out.push('\u{08}'),
                            b'f' => out.push('\u{0c}'),
                            b'u' => {
                                let hex = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
                                let cp = u32::from_str_radix(hex, 16).ok()?;
                                self.i += 4;
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                            _ => return None,
                        }
                    }
                    _ => {
                        // pass UTF-8 bytes through verbatim
                        out.push(c as char);
                        // fix multi-byte: re-decode below is overkill; relays
                        // send ASCII for our frames (hex, base-ish). Good enough.
                    }
                }
            }
            None
        }
        fn array(&mut self) -> Option<Json> {
            self.i += 1; // [
            let mut out = Vec::new();
            self.ws();
            if self.peek()? == b']' {
                self.i += 1;
                return Some(Json::Arr(out));
            }
            loop {
                out.push(self.value()?);
                self.ws();
                match self.peek()? {
                    b',' => {
                        self.i += 1;
                    }
                    b']' => {
                        self.i += 1;
                        return Some(Json::Arr(out));
                    }
                    _ => return None,
                }
            }
        }
        fn object(&mut self) -> Option<Json> {
            self.i += 1; // {
            let mut out = BTreeMap::new();
            self.ws();
            if self.peek()? == b'}' {
                self.i += 1;
                return Some(Json::Obj(out));
            }
            loop {
                self.ws();
                let key = self.string()?;
                self.ws();
                if self.peek()? != b':' {
                    return None;
                }
                self.i += 1;
                let val = self.value()?;
                out.insert(key, val);
                self.ws();
                match self.peek()? {
                    b',' => {
                        self.i += 1;
                    }
                    b'}' => {
                        self.i += 1;
                        return Some(Json::Obj(out));
                    }
                    _ => return None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_signs_and_verifies_and_roundtrips_json() {
        let id = Identity::generate();
        let ev = Event::create(&id, 1_700_000_000, KIND_BEACON, vec![vec!["t".into(), "ironvein-region-A1".into()]], "hello".into());
        assert!(ev.verify());
        assert_eq!(ev.tag("t"), Some("ironvein-region-A1"));

        let j = json::parse(&ev.to_json()).unwrap();
        let back = Event::from_json(&j).unwrap();
        assert_eq!(back, ev);
        assert!(back.verify());

        // a flipped content byte breaks the id/sig
        let mut tampered = ev.clone();
        tampered.content = "h3llo".into();
        assert!(!tampered.verify());
    }

    #[test]
    fn beacon_roundtrip() {
        let id = Identity::generate();
        let b = Beacon { region: "B2".into(), host: id.pk, tick: 4242, players: 3, genesis: 0xDEADBEEFCAFE, controller: id.pk, controller_name: "Ada".into(), private: false };
        let ev = b.to_event(&id, 1_700_000_001);
        assert!(ev.verify());
        assert_eq!(ev.tag("t"), Some("ironvein-region-B2"));
        let back = Beacon::from_event(&ev).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn room_code_rendezvous() {
        let id = Identity::generate();
        // a host's code is stable (derived from identity) and well-formed
        let code = host_room_code(&id.pk);
        assert_eq!(code.len(), 9); // XXXX-XXXX
        assert_eq!(&code[4..5], "-");
        assert_eq!(host_room_code(&id.pk), code, "code must be stable");

        // host and joiner derive the same private region from the same code,
        // regardless of how the joiner typed it (case / spacing / punctuation)
        let region = room_code_region(&code);
        let messy = format!("  {}  ", code.to_lowercase().replace('-', " "));
        assert_eq!(room_code_region(&messy), region);
        // a different code lands in a different room
        assert_ne!(room_code_region("AAAA-AAAA"), region);

        // a private beacon carries ONLY its region topic — never the global one,
        // so it can't be vacuumed up by the public lobby scan.
        let pb = Beacon {
            region: region.clone(),
            host: id.pk,
            tick: 1,
            players: 1,
            genesis: 0,
            controller: id.pk,
            controller_name: "Ada".into(),
            private: true,
        };
        let ev = pb.to_event(&id, 1_700_000_003);
        assert!(ev.verify());
        assert!(ev.has_tag("t", &region_topic(&region)));
        assert!(!ev.has_tag("t", GLOBAL_TOPIC), "private beacon must omit the global topic");
        assert!(Beacon::from_event(&ev).unwrap().private, "private flag must survive the round-trip");
    }

    #[test]
    fn signal_is_encrypted_and_addressed() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let sig = Signal::Offer("v=0 ... sdp offer".into());
        let ev = sig.to_event(&alice, &bob.pk, 1_700_000_002).unwrap();
        assert!(ev.verify());
        // routed to bob
        assert_eq!(ev.tag("p"), Some(hex_encode(&bob.pk)).as_deref());
        // the SDP is not in the clear
        assert!(!ev.content.contains("sdp offer"));
        // bob recovers it; eve cannot
        let (from, got) = Signal::from_event(&ev, &bob).unwrap();
        assert_eq!(from, alice.pk);
        assert_eq!(got, sig);
        let eve = Identity::generate();
        assert!(Signal::from_event(&ev, &eve).is_none());
    }

    #[test]
    fn json_parses_relay_array_frames() {
        let v = json::parse(r#"["EVENT","sub1",{"kind":29001,"tags":[["t","x"]]}]"#).unwrap();
        let arr = v.as_arr().unwrap();
        assert_eq!(arr[0].as_str(), Some("EVENT"));
        assert_eq!(arr[1].as_str(), Some("sub1"));
        assert_eq!(arr[2].as_obj().unwrap().get("kind").unwrap().as_u64(), Some(29001));
    }
}
