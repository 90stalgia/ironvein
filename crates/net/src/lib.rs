//! ironvein-net — peer-to-peer deterministic lockstep.
//!
//! The model (same family as the original 90s RTS netcode, modernized):
//!   * Nobody sends game state. Peers exchange only COMMANDS, scheduled
//!     `delay` ticks in the future. Tick T executes only when every roster
//!     member's commands for T have arrived — so every machine simulates the
//!     exact same world.
//!   * Late join = pause-the-world: the host freezes command issuance at a
//!     future tick, snapshots the (byte-identical-everywhere) world, hands it
//!     to the joiner, and everyone resumes with the newcomer in the roster.
//!   * Desyncs can't hide: peers gossip state hashes every 32 ticks.
//!   * Transport is pluggable behind the `Transport` trait: a full TCP mesh
//!     natively (LAN/VPN/port-forward), WebRTC data channels in the browser
//!     (signaled over Nostr relays). The session above the trait is sans-io
//!     and identical on both. See ARCHITECTURE.md.

pub mod crypto;
pub mod nostr;
pub mod protocol;
pub mod session;
pub mod signaling;
pub mod transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod transport_tcp;
#[cfg(target_arch = "wasm32")]
pub mod transport_webrtc;
#[cfg(target_arch = "wasm32")]
pub mod browser;

pub use crypto::{Identity, PubKey};
pub use protocol::{Envelope, Msg, PeerInfo, HOST_PID};
pub use session::{Joiner, Session, SessionKind, TICK_DT};
pub use transport::{ConnId, NullTransport, Transport, TransportEv};
