//! protocol.rs — the wire vocabulary of an IRONVEIN session.
//!
//! Framing: every message is `u32 little-endian length` + payload, and the
//! payload is always a signed `Envelope` — no peer speaks unsigned.
//! Encoding reuses the sim's zero-dependency serializer, so the wire
//! format and the save format are the same audited code path.

use crate::crypto::{self, Identity, PubKey};
use ironvein_sim::command::Command;
use ironvein_sim::ser::{DResult, DecodeErr, R, W};
use std::io::{Read, Write};

pub const HOST_PID: u8 = 0;
pub const MAX_FRAME: usize = 32 * 1024 * 1024; // snapshots fit comfortably

/// Domain tag for frame signatures — bumping it invalidates all old frames.
const MSG_TAG: &str = "ironvein/msg/v1";

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub pid: u8,
    pub name: String,
    pub color: u8,
    /// "ip:port" (TCP) or hex pubkey (WebRTC) another peer can dial;
    /// empty for bots and for the host's own entry.
    pub addr: String,
    /// Identity pubkey whose signature authorizes this pid's commands.
    /// Bots carry the driving host's key — their commands are the host's.
    pub key: PubKey,
    /// AI player driven by whoever currently hosts. Re-keyed to the new
    /// host on migration; never a candidate to become host.
    pub bot: bool,
}

impl PeerInfo {
    fn ser(&self, w: &mut W) {
        w.u8(self.pid);
        w.str(&self.name);
        w.u8(self.color);
        w.str(&self.addr);
        w.arr32(&self.key);
        w.bool(self.bot);
    }
    fn de(r: &mut R) -> DResult<PeerInfo> {
        Ok(PeerInfo {
            pid: r.u8()?,
            name: r.str()?,
            color: r.u8()?,
            addr: r.str()?,
            key: r.arr32()?,
            bot: r.bool()?,
        })
    }
}


// ----------------------------------------------------------------------
// The signed envelope: every frame on the wire is one of these
// ----------------------------------------------------------------------

/// SignedNetMessage: `payload` is an encoded `Msg`; `sig` is BIP-340
/// Schnorr by `sender` over a tagged hash of (sender ‖ seq ‖ payload).
/// `seq` is strictly increasing per sender (seeded from wall-clock millis,
/// so a later session always outranks a replayed earlier one).
#[derive(Clone, Debug)]
pub struct Envelope {
    pub sender: PubKey,
    pub seq: u64,
    pub payload: Vec<u8>,
    pub sig: [u8; 64],
}

impl Envelope {
    fn digest(sender: &PubKey, seq: u64, payload: &[u8]) -> [u8; 32] {
        crypto::tagged_hash(MSG_TAG, &[sender, &seq.to_le_bytes(), payload])
    }

    /// Sign and encode a message into wire bytes.
    pub fn seal(id: &Identity, seq: u64, msg: &Msg) -> Vec<u8> {
        let payload = msg.encode();
        let sig = id.sign(&Envelope::digest(&id.pk, seq, &payload));
        let mut w = W::new();
        w.arr32(&id.pk);
        w.u64(seq);
        w.bytes(&payload);
        w.buf.extend_from_slice(&sig);
        w.buf
    }

    pub fn decode(bytes: &[u8]) -> DResult<Envelope> {
        let mut r = R::new(bytes);
        let sender = r.arr32()?;
        let seq = r.u64()?;
        let payload = r.bytes()?;
        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(&r.arr32()?);
        sig[32..].copy_from_slice(&r.arr32()?);
        Ok(Envelope { sender, seq, payload, sig })
    }

    /// Does the signature check out against the claimed sender?
    pub fn verify(&self) -> bool {
        crypto::verify(&self.sender, &Envelope::digest(&self.sender, self.seq, &self.payload), &self.sig)
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    /// joiner -> host: knock knock.
    Hello { name: String, color: u8, listen_port: u16 },
    /// host -> joiner: your pid, the frozen world, and everyone to dial.
    Welcome { pid: u8, start_tick: u32, peers: Vec<PeerInfo>, snapshot: Vec<u8> },
    /// host -> joiner: no room / name trouble.
    Deny { reason: String },
    /// host -> all: stop sending commands for ticks >= at (a snapshot is coming).
    Freeze { at: u32 },
    /// host -> old peers: a settler arrived; require their commands from start_tick on.
    PeerJoined { info: PeerInfo, start_tick: u32 },
    /// mesh handshake: identifies an inbound connection.
    Dial { pid: u8 },
    /// the heartbeat of lockstep: player `pid`'s commands for `tick`.
    Cmds { tick: u32, pid: u8, cmds: Vec<Command> },
    /// periodic state checksum for desync detection.
    HashChk { tick: u32, hash: u64 },
    /// host -> all: `pid` left. Empty-fill their commands from `from`;
    /// use `backfill` verbatim for earlier unexecuted ticks (host's view is canon).
    Left { pid: u8, from: u32, backfill: Vec<(u32, Vec<Command>)> },
    /// HOST MIGRATION, new host -> all survivors: the elected successor
    /// (lowest surviving pid) declares its own frozen world canonical and
    /// ships it, exactly like a join `Welcome`. Everyone loads `snapshot`,
    /// retires the dead `old` host (its base persists in the bytes),
    /// re-keys/`bots` to the new host, and resumes at `resume_tick`. Loading
    /// identical bytes is bulletproof where command-replay is not: survivors
    /// at different ticks all converge to one state. Authorized by the new
    /// host's key. (Mirrors "the host's record is canon" for departures.)
    MigrateResume { old: u8, resume_tick: u32, bots: Vec<u8>, snapshot: Vec<u8> },
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W::new();
        match self {
            Msg::Hello { name, color, listen_port } => {
                w.u8(0);
                w.str(name);
                w.u8(*color);
                w.u16(*listen_port);
            }
            Msg::Welcome { pid, start_tick, peers, snapshot } => {
                w.u8(1);
                w.u8(*pid);
                w.u32(*start_tick);
                w.u8(peers.len() as u8);
                for p in peers {
                    p.ser(&mut w);
                }
                w.bytes(snapshot);
            }
            Msg::Deny { reason } => {
                w.u8(2);
                w.str(reason);
            }
            Msg::Freeze { at } => {
                w.u8(3);
                w.u32(*at);
            }
            Msg::PeerJoined { info, start_tick } => {
                w.u8(4);
                info.ser(&mut w);
                w.u32(*start_tick);
            }
            Msg::Dial { pid } => {
                w.u8(5);
                w.u8(*pid);
            }
            Msg::Cmds { tick, pid, cmds } => {
                w.u8(6);
                w.u32(*tick);
                w.u8(*pid);
                w.u16(cmds.len() as u16);
                for c in cmds {
                    c.ser(&mut w);
                }
            }
            Msg::HashChk { tick, hash } => {
                w.u8(7);
                w.u32(*tick);
                w.u64(*hash);
            }
            Msg::Left { pid, from, backfill } => {
                w.u8(8);
                w.u8(*pid);
                w.u32(*from);
                w.u16(backfill.len() as u16);
                for (t, cmds) in backfill {
                    w.u32(*t);
                    w.u16(cmds.len() as u16);
                    for c in cmds {
                        c.ser(&mut w);
                    }
                }
            }
            Msg::MigrateResume { old, resume_tick, bots, snapshot } => {
                w.u8(9);
                w.u8(*old);
                w.u32(*resume_tick);
                w.u8(bots.len() as u8);
                for b in bots {
                    w.u8(*b);
                }
                w.bytes(snapshot);
            }
        }
        w.buf
    }

    pub fn decode(bytes: &[u8]) -> DResult<Msg> {
        let mut r = R::new(bytes);
        let tag = r.u8()?;
        Ok(match tag {
            0 => Msg::Hello { name: r.str()?, color: r.u8()?, listen_port: r.u16()? },
            1 => {
                let pid = r.u8()?;
                let start_tick = r.u32()?;
                let n = r.u8()? as usize;
                let mut peers = Vec::with_capacity(n);
                for _ in 0..n {
                    peers.push(PeerInfo::de(&mut r)?);
                }
                let snapshot = r.bytes()?;
                Msg::Welcome { pid, start_tick, peers, snapshot }
            }
            2 => Msg::Deny { reason: r.str()? },
            3 => Msg::Freeze { at: r.u32()? },
            4 => Msg::PeerJoined { info: PeerInfo::de(&mut r)?, start_tick: r.u32()? },
            5 => Msg::Dial { pid: r.u8()? },
            6 => {
                let tick = r.u32()?;
                let pid = r.u8()?;
                let n = r.u16()? as usize;
                let mut cmds = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    cmds.push(Command::de(&mut r)?);
                }
                Msg::Cmds { tick, pid, cmds }
            }
            7 => Msg::HashChk { tick: r.u32()?, hash: r.u64()? },
            8 => {
                let pid = r.u8()?;
                let from = r.u32()?;
                let n = r.u16()? as usize;
                let mut backfill = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    let t = r.u32()?;
                    let m = r.u16()? as usize;
                    let mut cmds = Vec::with_capacity(m.min(1024));
                    for _ in 0..m {
                        cmds.push(Command::de(&mut r)?);
                    }
                    backfill.push((t, cmds));
                }
                Msg::Left { pid, from, backfill }
            }
            9 => {
                let old = r.u8()?;
                let resume_tick = r.u32()?;
                let nb = r.u8()? as usize;
                let mut bots = Vec::with_capacity(nb);
                for _ in 0..nb {
                    bots.push(r.u8()?);
                }
                Msg::MigrateResume { old, resume_tick, bots, snapshot: r.bytes()? }
            }
            _ => return Err(DecodeErr),
        })
    }
}

/// Length-prefix and write one raw frame (an encoded `Envelope`).
pub fn write_frame(stream: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() as u32).to_le_bytes();
    stream.write_all(&len)?;
    stream.write_all(payload)?;
    Ok(())
}

/// Read one length-prefixed raw frame. Decoding/verification is the
/// session's business, not the transport's.
pub fn read_frame(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut lb = [0u8; 4];
    stream.read_exact(&mut lb)?;
    let len = u32::from_le_bytes(lb) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}
