//! session.rs — the lockstep engine room, sans-io and signature-checked.
//!
//! The session owns no sockets and spawns no threads: it consumes
//! `TransportEv`s from a `Transport` and answers with frames. That is what
//! lets the identical lockstep logic run over native TCP (reader threads
//! behind the transport) and browser WebRTC (single-threaded, polled from
//! the frame loop).
//!
//! Every frame on the wire is a signed `Envelope`. The rules, enforced
//! here before anything reaches the sim:
//!   * the BIP-340 signature must verify against the envelope's sender key;
//!   * `seq` must be strictly increasing per sender (replay shield);
//!   * a link is locked to the first key that speaks on it;
//!   * `Cmds{pid}` requires `roster[pid].key == sender` — you cannot issue
//!     orders for a settlement whose key you don't hold (bots carry the
//!     driving host's key);
//!   * Freeze / PeerJoined / Left are only honored from the host's key.
//! Anything failing a rule is dropped silently — and a modified sim that
//! accepted it anyway desyncs out of the world within 32 ticks.

use crate::crypto::{self, Identity, PubKey, ZERO_KEY};
use crate::protocol::{Envelope, Msg, PeerInfo, HOST_PID};
use crate::transport::{ConnId, NullTransport, Transport, TransportEv};
use ironvein_sim::command::Command;
use ironvein_sim::{Pid, World};
use std::collections::{BTreeMap, VecDeque};
#[cfg(not(target_arch = "wasm32"))]
use std::io;

pub const TICK_DT: f64 = 1.0 / ironvein_sim::TICK_HZ as f64;
const DELAY: u32 = 3; // input delay in ticks (300ms): the lockstep latency budget
const HASH_EVERY: u32 = 32;
const HISTORY_KEEP: u32 = 128;
const MAX_CATCHUP: u32 = 8;
/// Anyone silent this long is treated as gone (a half-dead link would
/// otherwise stall the world forever).
const PEER_TIMEOUT_S: f64 = 30.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionKind {
    Solo,
    Host,
    Client,
}

/// In-flight host migration. While `Some`, the world is frozen on this peer
/// until the canonical snapshot arrives (or, for the arbiter, is shipped).
struct Migration {
    /// deterministically elected successor = lowest surviving non-bot pid
    /// (kept for status/telemetry; the resume verdict is re-validated on
    /// arrival rather than trusted from here)
    #[allow(dead_code)]
    new_host: u8,
}

pub struct Session {
    pub world: World,
    pub kind: SessionKind,
    pub my_pid: Pid,
    pub my_name: String,
    pub my_color: u8,
    pub roster: BTreeMap<u8, PeerInfo>,
    pub status: String,
    pub desync_at: Option<u32>,
    /// interpolation alpha for the renderer
    pub accum: f64,

    identity: Identity,
    /// next outbound envelope sequence number (strictly increasing; seeded
    /// from wall-clock millis so a new session outranks replayed old frames)
    seq: u64,
    /// highest seq seen per sender key
    peer_seq: BTreeMap<PubKey, u64>,
    /// each link is locked to the first key that speaks on it
    conn_key: BTreeMap<ConnId, PubKey>,

    transport: Box<dyn Transport>,
    /// who paces the clock and arbitrates departures (rebound on migration)
    host_pid: u8,
    pid_of: BTreeMap<ConnId, u8>,
    conn_of: BTreeMap<u8, ConnId>,
    /// Dial claims that arrived before the matching PeerJoined: (conn, key, pid)
    pending_dials: Vec<(ConnId, PubKey, u8)>,

    delay: u32,
    next_send: u32,
    pending: BTreeMap<u32, BTreeMap<u8, Vec<Command>>>,
    history: BTreeMap<u32, BTreeMap<u8, Vec<Command>>>,
    /// departed peers whose final commands must still execute: pid -> first empty tick
    ghosts: BTreeMap<u8, u32>,
    /// clients pace themselves off the host: host commands exist for ticks < host_seen
    host_seen: u32,
    /// wall clock, sampled from the transport each update (watchdogs only —
    /// the sim is paced by `dt`)
    now: f64,
    last_heard: BTreeMap<u8, f64>,
    freeze_at: Option<u32>,
    /// host: joiners knocking, waiting for the freeze point
    lobby: VecDeque<(ConnId, PubKey, String, u8, u16)>,
    local_q: Vec<Command>,
    bot_q: BTreeMap<u8, Vec<Command>>,
    my_hashes: VecDeque<(u32, u64)>,
    host_gone: bool,
    migration: Option<Migration>,
}

impl Session {
    fn base(world: World, kind: SessionKind, identity: Identity, name: &str, color: u8) -> Session {
        Session {
            world,
            kind,
            my_pid: 0,
            my_name: name.to_string(),
            my_color: color,
            roster: BTreeMap::new(),
            status: String::new(),
            desync_at: None,
            accum: 0.0,
            seq: crypto::unix_millis().wrapping_mul(1024),
            peer_seq: BTreeMap::new(),
            conn_key: BTreeMap::new(),
            identity,
            transport: Box::new(NullTransport),
            host_pid: HOST_PID,
            pid_of: BTreeMap::new(),
            conn_of: BTreeMap::new(),
            pending_dials: Vec::new(),
            delay: DELAY,
            next_send: 0,
            pending: BTreeMap::new(),
            history: BTreeMap::new(),
            ghosts: BTreeMap::new(),
            host_seen: 0,
            now: 0.0,
            last_heard: BTreeMap::new(),
            freeze_at: None,
            lobby: VecDeque::new(),
            local_q: Vec::new(),
            bot_q: BTreeMap::new(),
            my_hashes: VecDeque::new(),
            host_gone: false,
            migration: None,
        }
    }

    pub fn my_key(&self) -> PubKey {
        self.identity.pk
    }

    fn seal(&mut self, msg: &Msg) -> Vec<u8> {
        let bytes = Envelope::seal(&self.identity, self.seq, msg);
        self.seq += 1;
        bytes
    }

    fn send_to(&mut self, conn: ConnId, msg: &Msg) {
        let bytes = self.seal(msg);
        self.transport.send(conn, &bytes);
    }

    fn broadcast(&mut self, msg: &Msg, except: Option<u8>) {
        // seal once: every recipient sees the same (sender, seq, sig)
        let bytes = self.seal(msg);
        let conns: Vec<(u8, ConnId)> = self.conn_of.iter().map(|(p, c)| (*p, *c)).collect();
        for (pid, conn) in conns {
            if Some(pid) == except {
                continue;
            }
            self.transport.send(conn, &bytes);
        }
    }

    fn bind(&mut self, conn: ConnId, pid: u8) {
        self.pid_of.insert(conn, pid);
        self.conn_of.insert(pid, conn);
        self.last_heard.insert(pid, self.now);
    }

    fn host_key(&self) -> PubKey {
        self.roster.get(&self.host_pid).map(|p| p.key).unwrap_or(ZERO_KEY)
    }

    fn seed_empty(&mut self, from: u32, count: u32) {
        let pids: Vec<u8> = self.roster.keys().copied().collect();
        for t in from..from + count {
            let entry = self.pending.entry(t).or_default();
            for &p in &pids {
                entry.entry(p).or_default();
            }
        }
    }

    /// pid for a (re)joining settler. Reclaim requires holding the
    /// settlement's key (legacy zero-key settlements reclaim by name);
    /// pids currently online are never reclaimable — that also stops
    /// anyone "joining as" a live bot. Otherwise: lowest free slot.
    fn pid_for(world: &World, roster: &BTreeMap<u8, PeerInfo>, name: &str, key: &PubKey) -> Option<u8> {
        for (i, p) in world.players.iter().enumerate() {
            let pid = i as u8;
            if !p.joined || roster.contains_key(&pid) {
                continue;
            }
            let keyed = *key != ZERO_KEY && p.key == *key && p.name == name;
            let legacy = p.key == ZERO_KEY && p.name == name;
            if keyed || legacy {
                return Some(pid);
            }
        }
        (0..ironvein_sim::MAX_PLAYERS as u8).find(|pid| {
            !roster.contains_key(pid)
                && world
                    .players
                    .get(*pid as usize)
                    .map(|p| !p.joined)
                    .unwrap_or(true)
        })
    }

    // ------------------------------------------------------------------
    // Constructors
    // ------------------------------------------------------------------

    pub fn solo(world: World, identity: Identity, name: &str, color: u8, bot_names: &[String]) -> Session {
        let mut s = Session::base(world, SessionKind::Solo, identity, name, color);
        let my_key = s.identity.pk;
        s.my_pid = Session::pid_for(&s.world, &s.roster, name, &my_key).unwrap_or(0);
        s.roster.insert(
            s.my_pid,
            PeerInfo { pid: s.my_pid, name: name.into(), color, addr: String::new(), key: my_key, bot: false },
        );
        for bn in bot_names {
            if let Some(bpid) = Session::pid_for(&s.world, &s.roster, bn, &ZERO_KEY) {
                // bots sign under the driving host's key
                s.roster.insert(
                    bpid,
                    PeerInfo { pid: bpid, name: bn.clone(), color: bpid % 8, addr: String::new(), key: my_key, bot: true },
                );
                s.bot_q.insert(bpid, Vec::new());
            }
        }
        let t0 = s.world.tick;
        s.seed_empty(t0, s.delay);
        s.next_send = t0 + s.delay;
        s.local_q.push(Command::Join { name: name.into(), color, key: my_key });
        s.status = "offline world".into();
        s
    }

    /// Host over any transport (the WebRTC path constructs sessions here).
    pub fn host_on(
        transport: Box<dyn Transport>,
        world: World,
        identity: Identity,
        name: &str,
        color: u8,
        bot_names: &[String],
    ) -> Session {
        let mut s = Session::solo(world, identity, name, color, bot_names);
        s.kind = SessionKind::Host;
        s.transport = transport;
        s.status = "hosting".into();
        s
    }

    /// Host on the native TCP mesh.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn host(
        world: World,
        identity: Identity,
        port: u16,
        name: &str,
        color: u8,
        bot_names: &[String],
    ) -> io::Result<Session> {
        let mesh = crate::transport_tcp::TcpMesh::listen(port)?;
        let mut s = Session::host_on(Box::new(mesh), world, identity, name, color, bot_names);
        s.status = format!("hosting on port {port} — friends join with --join <your-ip>:{port}");
        Ok(s)
    }

    /// Join over the native TCP mesh. Blocks until the freeze+welcome dance
    /// is done (native convenience; the pollable path is `Joiner`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn join(host_addr: &str, identity: Identity, listen_port: u16, name: &str, color: u8) -> io::Result<Session> {
        let mesh = crate::transport_tcp::TcpMesh::listen(listen_port)?;
        let mut joiner = Joiner::new(Box::new(mesh), host_addr, identity, name, color)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "dial failed"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(result) = joiner.poll() {
                return result.map_err(|e| io::Error::new(io::ErrorKind::Other, e));
            }
            if std::time::Instant::now() > deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no Welcome from host"));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A verified `Welcome` arrived: build the client session around the
    /// frozen snapshot and mesh out to every peer in the roster. `leftover`
    /// is whatever followed the Welcome in the same poll batch (the host
    /// resumes broadcasting Cmds immediately) — it must not be dropped.
    #[allow(clippy::too_many_arguments)]
    fn from_welcome(
        transport: Box<dyn Transport>,
        host_conn: ConnId,
        identity: Identity,
        seq: u64,
        host_key: PubKey,
        host_seq: u64,
        pid: u8,
        start_tick: u32,
        peers: Vec<PeerInfo>,
        snapshot: Vec<u8>,
        name: &str,
        color: u8,
        leftover: Vec<TransportEv>,
    ) -> Result<Session, String> {
        // the roster's host entry must agree with the key that signed the
        // Welcome, or someone is impersonating
        let listed = peers.iter().find(|p| p.pid == HOST_PID).map(|p| p.key);
        if listed != Some(host_key) {
            return Err("host key mismatch in Welcome".into());
        }
        let world = World::load_bytes(&snapshot).map_err(|_| "bad snapshot".to_string())?;
        let mut s = Session::base(world, SessionKind::Client, identity, name, color);
        let my_key = s.identity.pk;
        s.seq = seq;
        s.transport = transport;
        // Stamp the clock from the transport BEFORE any bind(): bind() records
        // last_heard = self.now, and a WebRTC transport's now_s() is wall-clock
        // (~1.7e9 s). If we left now at 0, the very first host_watchdog tick
        // would see "host silent for a billion seconds" and falsely migrate,
        // abandoning a perfectly live connection the instant we joined.
        s.now = s.transport.now_s();
        s.my_pid = pid;
        s.roster.insert(pid, PeerInfo { pid, name: name.into(), color, addr: String::new(), key: my_key, bot: false });
        s.conn_key.insert(host_conn, host_key);
        s.peer_seq.insert(host_key, host_seq);
        s.bind(host_conn, HOST_PID);

        // mesh out to everyone else
        for p in peers {
            s.roster.insert(p.pid, p.clone());
            if p.pid == HOST_PID || p.pid == pid || p.addr.is_empty() {
                continue; // host already connected; bots have no address
            }
            match s.transport.dial(&p.addr) {
                Some(conn) => {
                    s.conn_key.insert(conn, p.key);
                    s.bind(conn, p.pid);
                    // queued by the transport until the dial completes
                    s.send_to(conn, &Msg::Dial { pid });
                }
                None => {
                    s.status = format!("could not reach peer {} at {}", p.pid, p.addr);
                }
            }
        }

        s.seed_empty(start_tick, s.delay);
        s.next_send = start_tick + s.delay;
        s.host_seen = start_tick + s.delay;
        s.local_q.push(Command::Join { name: name.into(), color, key: my_key });
        s.status = format!("joined as player {pid}");
        for ev in leftover {
            s.handle_ev(ev);
        }
        Ok(s)
    }

    // ------------------------------------------------------------------
    // Command intake
    // ------------------------------------------------------------------

    /// Queue a command from the local player. It will be scheduled `delay`
    /// ticks ahead and executed simultaneously on every peer.
    pub fn queue(&mut self, cmd: Command) {
        self.local_q.push(cmd);
    }

    /// Queue a command on behalf of a bot (solo/host only — bots live where
    /// the world was born and their commands replicate like anyone else's).
    pub fn queue_as(&mut self, pid: u8, cmd: Command) {
        self.bot_q.entry(pid).or_default().push(cmd);
    }

    /// Pids of bots this session is responsible for driving.
    pub fn bot_pids(&self) -> Vec<u8> {
        if self.kind == SessionKind::Client {
            Vec::new()
        } else {
            self.bot_q.keys().copied().collect()
        }
    }

    /// Interpolation factor for the renderer: how far we are into the
    /// current tick, 0..1.
    pub fn alpha(&self) -> f32 {
        (self.accum / TICK_DT).clamp(0.0, 1.0) as f32
    }

    pub fn peer_count(&self) -> usize {
        self.roster.len()
    }

    // ------------------------------------------------------------------
    // The pump
    // ------------------------------------------------------------------

    /// Drive the session: pump the network, run due ticks, ship our commands.
    /// Returns how many simulation ticks were executed.
    pub fn update(&mut self, dt: f64) -> u32 {
        self.now = self.transport.now_s();
        for ev in self.transport.poll() {
            self.handle_ev(ev);
        }
        // While a host migration is in flight the world is frozen on this
        // peer until the canonical snapshot is applied (the arbiter applies
        // it inline in begin_migration; others on MigrateResume).
        if self.migration.is_some() {
            self.accum = 0.0;
            return 0;
        }
        if self.kind == SessionKind::Host {
            self.watchdog();
            self.try_admit();
        } else if self.kind == SessionKind::Client {
            self.host_watchdog();
        }
        self.ensure_sent();

        if self.host_gone || self.desync_at.is_some() {
            self.accum = 0.0;
            return 0;
        }

        self.accum += dt;
        let mut stepped = 0;
        while self.accum >= TICK_DT && stepped < MAX_CATCHUP {
            if !self.ready(self.world.tick) {
                // waiting on the network: don't bank unbounded time debt
                self.accum = self.accum.min(TICK_DT);
                break;
            }
            self.advance();
            self.ensure_sent();
            self.accum -= TICK_DT;
            stepped += 1;
        }
        if stepped == MAX_CATCHUP {
            self.accum = 0.0;
        }
        stepped
    }

    /// Ship Cmds frames for every tick we owe the mesh.
    fn ensure_sent(&mut self) {
        loop {
            let t = self.next_send;
            if t > self.world.tick + self.delay {
                break;
            }
            if let Some(fa) = self.freeze_at {
                if t >= fa {
                    break;
                }
            }
            // clients pace off the host so a join freeze can never be outrun
            if self.kind == SessionKind::Client && t >= self.host_seen {
                break;
            }
            let mine: Vec<Command> = std::mem::take(&mut self.local_q);
            self.pending.entry(t).or_default().insert(self.my_pid, mine.clone());
            if self.kind != SessionKind::Solo {
                self.broadcast(&Msg::Cmds { tick: t, pid: self.my_pid, cmds: mine }, None);
            }
            let bots: Vec<u8> = self.bot_q.keys().copied().collect();
            for b in bots {
                let bc: Vec<Command> = std::mem::take(self.bot_q.get_mut(&b).unwrap());
                self.pending.entry(t).or_default().insert(b, bc.clone());
                if self.kind != SessionKind::Solo {
                    self.broadcast(&Msg::Cmds { tick: t, pid: b, cmds: bc }, None);
                }
            }
            self.next_send = t + 1;
        }
    }

    /// Can tick `t` execute? Everyone in the roster — and every ghost whose
    /// final commands haven't run out yet — must be accounted for.
    fn ready(&self, t: u32) -> bool {
        let Some(row) = self.pending.get(&t) else { return false };
        for pid in self.roster.keys() {
            if !row.contains_key(pid) {
                return false;
            }
        }
        for (pid, from) in &self.ghosts {
            if t < *from && !row.contains_key(pid) {
                return false;
            }
        }
        true
    }

    /// Execute one tick: identical command set, identical order, everywhere.
    fn advance(&mut self) {
        let t = self.world.tick;
        let row = self.pending.remove(&t).unwrap_or_default();
        let mut kept: BTreeMap<u8, Vec<Command>> = BTreeMap::new();
        for (pid, cmds) in row {
            let live = self.roster.contains_key(&pid);
            let ghost = self.ghosts.get(&pid).map(|from| t < *from).unwrap_or(false);
            if live || ghost {
                kept.insert(pid, cmds);
            }
        }
        let flat: Vec<(u8, Command)> = kept
            .iter()
            .flat_map(|(p, cs)| cs.iter().cloned().map(move |c| (*p, c)))
            .collect();
        self.world.step(&flat);
        self.history.insert(t, kept);

        // pruning
        let cutoff = self.world.tick.saturating_sub(HISTORY_KEEP);
        while let Some((&k, _)) = self.history.iter().next() {
            if k < cutoff { self.history.remove(&k); } else { break }
        }
        while let Some((&k, _)) = self.pending.iter().next() {
            if k < self.world.tick { self.pending.remove(&k); } else { break }
        }
        let done: Vec<u8> = self.ghosts.iter().filter(|(_, f)| self.world.tick >= **f).map(|(p, _)| *p).collect();
        for p in done { self.ghosts.remove(&p); }

        // desync sentinel
        if self.world.tick % HASH_EVERY == 0 {
            let h = self.world.hash();
            self.my_hashes.push_back((self.world.tick, h));
            while self.my_hashes.len() > 64 {
                self.my_hashes.pop_front();
            }
            if self.kind != SessionKind::Solo {
                self.broadcast(&Msg::HashChk { tick: self.world.tick, hash: h }, None);
            }
        }
    }

    // ------------------------------------------------------------------
    // Event handling
    // ------------------------------------------------------------------

    fn handle_ev(&mut self, ev: TransportEv) {
        match ev {
            TransportEv::Connected { .. } => {
                // links are anonymous until their first signed frame
            }
            TransportEv::Data { conn, bytes } => {
                // the verification gauntlet — fail any check, vanish
                let Ok(env) = Envelope::decode(&bytes) else { return };
                if !env.verify() {
                    return;
                }
                if let Some(last) = self.peer_seq.get(&env.sender) {
                    if env.seq <= *last {
                        return; // replayed or reordered: drop
                    }
                }
                match self.conn_key.get(&conn) {
                    Some(locked) if *locked != env.sender => return, // link is not yours
                    Some(_) => {}
                    None => {
                        self.conn_key.insert(conn, env.sender);
                    }
                }
                self.peer_seq.insert(env.sender, env.seq);
                let Ok(msg) = Msg::decode(&env.payload) else { return };
                if let Some(&pid) = self.pid_of.get(&conn) {
                    self.last_heard.insert(pid, self.now);
                    self.handle_msg(pid, env.sender, msg);
                } else {
                    self.first_frame(conn, env.sender, msg);
                }
            }
            TransportEv::Closed { conn } => {
                self.conn_key.remove(&conn);
                self.pending_dials.retain(|(c, _, _)| *c != conn);
                if let Some(pid) = self.pid_of.remove(&conn) {
                    self.conn_of.remove(&pid);
                    self.handle_down(pid);
                }
            }
        }
    }

    /// First frame on an anonymous link: Hello (joiner) or Dial (mesh peer).
    fn first_frame(&mut self, conn: ConnId, sender: PubKey, msg: Msg) {
        match msg {
            Msg::Hello { name, color, listen_port } => {
                if self.kind != SessionKind::Host {
                    self.send_to(conn, &Msg::Deny { reason: "not the host".into() });
                    return;
                }
                if self.roster.values().any(|p| p.key == sender) {
                    self.send_to(conn, &Msg::Deny { reason: "this key is already in the world".into() });
                    return;
                }
                self.lobby.push_back((conn, sender, name, color, listen_port));
                if self.freeze_at.is_none() {
                    let fa = self.world.tick + 3 * self.delay + 3;
                    self.freeze_at = Some(fa);
                    self.broadcast(&Msg::Freeze { at: fa }, None);
                    self.status = "a settler is arriving — world pausing…".into();
                }
            }
            Msg::Dial { pid } => {
                self.try_bind_dial(conn, sender, pid);
            }
            _ => {}
        }
    }

    /// A meshing peer claims a pid: only honor it if the roster says that
    /// pid belongs to their key. If the PeerJoined hasn't reached us yet,
    /// park the claim and retry after the next join.
    fn try_bind_dial(&mut self, conn: ConnId, sender: PubKey, pid: u8) {
        match self.roster.get(&pid) {
            Some(info) if info.key == sender => self.bind(conn, pid),
            Some(_) => self.transport.close(conn), // claiming someone else's pid
            None => self.pending_dials.push((conn, sender, pid)),
        }
    }

    fn handle_msg(&mut self, from_pid: u8, sender: PubKey, msg: Msg) {
        match msg {
            Msg::Cmds { tick, pid, cmds } => {
                // authorship: the envelope key must own the pid it commands
                if self.roster.get(&pid).map(|p| p.key) != Some(sender) {
                    return;
                }
                if tick >= self.world.tick {
                    self.pending.entry(tick).or_default().insert(pid, cmds);
                    if from_pid == self.host_pid && self.kind == SessionKind::Client {
                        self.host_seen = self.host_seen.max(tick + 1);
                    }
                }
            }
            Msg::HashChk { tick, hash } => {
                if self.roster.get(&from_pid).map(|p| p.key) != Some(sender) {
                    return;
                }
                if let Some(&(_, mine)) = self.my_hashes.iter().find(|(t, _)| *t == tick) {
                    if mine != hash && self.desync_at.is_none() {
                        self.desync_at = Some(tick);
                        self.status = format!("DESYNC at tick {tick} vs player {from_pid} — world halted");
                    }
                }
            }
            // world-control verdicts: only the host's key may speak them
            Msg::Freeze { at } => {
                if sender != self.host_key() {
                    return;
                }
                self.freeze_at = Some(at);
                self.status = "world pausing — someone is joining…".into();
            }
            Msg::PeerJoined { info, start_tick } => {
                if sender != self.host_key() {
                    return;
                }
                self.apply_join(info, start_tick);
            }
            Msg::Left { pid, from, backfill } => {
                if sender != self.host_key() {
                    return;
                }
                for (t, cmds) in backfill {
                    if t >= self.world.tick {
                        self.pending.entry(t).or_default().insert(pid, cmds);
                    }
                }
                self.depart(pid, from);
            }
            Msg::MigrateResume { old, resume_tick, bots, snapshot } => {
                // Accept the canonical resume if it's a legitimate election
                // outcome — even if we haven't noticed the host drop yet
                // (detection order varies). The sender must be the pid we'd
                // elect ourselves, signed by that pid's key, retiring the
                // host we currently follow.
                let legit = old == self.host_pid
                    && self.elect_host(old) == Some(from_pid)
                    && self.roster.get(&from_pid).map(|p| p.key) == Some(sender);
                if legit {
                    self.migration = None;
                    self.apply_migration(old, from_pid, resume_tick, bots, snapshot);
                }
            }
            Msg::Deny { reason } => {
                self.status = format!("denied: {reason}");
            }
            _ => {}
        }
    }

    /// A new settler is in: everyone runs this with identical arguments.
    fn apply_join(&mut self, info: PeerInfo, start_tick: u32) {
        let pid = info.pid;
        self.roster.insert(pid, info);
        // a parked Dial claim may now be verifiable
        let parked: Vec<(ConnId, PubKey, u8)> = std::mem::take(&mut self.pending_dials);
        for (conn, key, claimed) in parked {
            self.try_bind_dial(conn, key, claimed);
        }
        // The frozen window [start, start+delay) is empty BY DEFINITION for
        // every pid — overwrite, so all peers agree byte-for-byte.
        let pids: Vec<u8> = self.roster.keys().copied().collect();
        for t in start_tick..start_tick + self.delay {
            let row = self.pending.entry(t).or_default();
            for &p in &pids {
                row.insert(p, Vec::new());
            }
        }
        self.next_send = self.next_send.max(start_tick + self.delay);
        self.host_seen = self.host_seen.max(start_tick + self.delay);
        self.freeze_at = None;
        self.status = format!("{} joined the world", self.roster[&pid].name);
    }

    fn depart(&mut self, pid: u8, from: u32) {
        if self.roster.remove(&pid).is_some() {
            if self.world.tick < from {
                self.ghosts.insert(pid, from);
            }
            if let Some(conn) = self.conn_of.remove(&pid) {
                self.pid_of.remove(&conn);
                self.conn_key.remove(&conn);
                self.transport.close(conn);
            }
            self.status = format!("player {pid} left — their base stands");
        }
    }

    fn handle_down(&mut self, pid: u8) {
        match self.kind {
            SessionKind::Host => {
                if !self.roster.contains_key(&pid) {
                    return;
                }
                // Host's command record is canon: find the first tick we
                // don't have from them, backfill everyone up to it.
                let mut from = self.world.tick;
                while self.pending.get(&from).map(|r| r.contains_key(&pid)).unwrap_or(false) {
                    from += 1;
                }
                let lo = self.world.tick.saturating_sub(16);
                let mut backfill = Vec::new();
                for t in lo..from {
                    let cmds = self
                        .pending
                        .get(&t)
                        .and_then(|r| r.get(&pid))
                        .or_else(|| self.history.get(&t).and_then(|r| r.get(&pid)))
                        .cloned();
                    if let Some(c) = cmds {
                        backfill.push((t, c));
                    }
                }
                self.broadcast(&Msg::Left { pid, from, backfill }, None);
                self.depart(pid, from);
            }
            SessionKind::Client => {
                if pid == self.host_pid {
                    // the arbiter vanished: elect a successor and migrate the
                    // world rather than letting it die
                    self.begin_migration();
                } else if self.migration.as_ref().map(|m| m.new_host) == Some(pid) {
                    // the peer we just elected also dropped — re-elect
                    self.migration = None;
                    self.begin_migration();
                }
                // other peers dropping in normal play: wait for the host's Left verdict
            }
            SessionKind::Solo => {}
        }
    }

    // ------------------------------------------------------------------
    // Host migration — the world outlives any single host
    // ------------------------------------------------------------------

    /// Deterministically elect the new host: the lowest surviving non-bot
    /// pid, excluding the host that just vanished. Every survivor computes
    /// the identical answer from the identical roster. `None` only if no
    /// human remains.
    fn elect_host(&self, old: u8) -> Option<u8> {
        self.roster
            .iter()
            .filter(|(pid, p)| **pid != old && !p.bot)
            .map(|(pid, _)| *pid)
            .min()
    }

    /// The host's link is gone. Elect a successor; if it's me, ship my world
    /// as the new canon. Deterministic across all survivors.
    fn begin_migration(&mut self) {
        let old = self.host_pid;
        let Some(new_host) = self.elect_host(old) else {
            self.host_gone = true;
            self.status = "host left and no peers remain — world paused (your save persists)".into();
            return;
        };

        // drop the dead host's link bookkeeping (its socket is already gone)
        if let Some(conn) = self.conn_of.remove(&old) {
            self.pid_of.remove(&conn);
            self.conn_key.remove(&conn);
        }

        self.migration = Some(Migration { new_host });
        if new_host == self.my_pid {
            // I am the successor: my frozen world is now canon. Snapshot it
            // and hand it to every survivor, then adopt it myself.
            self.status = "host vanished — taking over the world…".into();
            let snapshot = self.world.save_bytes();
            let resume_tick = self.world.tick;
            let bots: Vec<u8> = self.roster.iter().filter(|(_, p)| p.bot).map(|(pid, _)| *pid).collect();
            self.broadcast(
                &Msg::MigrateResume { old, resume_tick, bots: bots.clone(), snapshot: snapshot.clone() },
                None,
            );
            self.apply_migration(old, self.my_pid, resume_tick, bots, snapshot);
        } else {
            self.status = format!("host vanished — player {new_host} is taking over…");
            // wait for their MigrateResume
        }
    }

    /// Adopt the canonical post-migration world. Every survivor runs this
    /// with the identical snapshot, so they converge byte-for-byte no matter
    /// what tick each was frozen at. Loading a snapshot is bulletproof where
    /// replaying a command gap (across roster changes) is not.
    fn apply_migration(
        &mut self,
        old: u8,
        new_host: u8,
        _resume_tick: u32,
        bots: Vec<u8>,
        snapshot: Vec<u8>,
    ) {
        let Ok(world) = World::load_bytes(&snapshot) else {
            self.status = "migration snapshot was corrupt — world halted".into();
            self.desync_at = Some(self.world.tick);
            return;
        };
        self.world = world;
        let new_key = self.roster.get(&new_host).map(|p| p.key).unwrap_or(ZERO_KEY);

        // the dead host leaves the live mesh; its settlement persists in the
        // snapshot bytes and can be reclaimed later by its key
        self.roster.remove(&old);
        if let Some(conn) = self.conn_of.remove(&old) {
            self.pid_of.remove(&conn);
            self.conn_key.remove(&conn);
        }
        // bots are now authored & driven by the new host
        for &b in &bots {
            if let Some(p) = self.roster.get_mut(&b) {
                p.key = new_key;
            }
        }
        self.host_pid = new_host;

        // restart the lockstep timeline cleanly around the snapshot's tick,
        // exactly like a join: an empty input window, then live again
        self.pending.clear();
        self.history.clear();
        self.ghosts.clear();
        self.local_q.clear();
        let resume_from = self.world.tick;
        let pids: Vec<u8> = self.roster.keys().copied().collect();
        for t in resume_from..resume_from + self.delay {
            let row = self.pending.entry(t).or_default();
            for &p in &pids {
                row.insert(p, Vec::new());
            }
        }
        self.next_send = resume_from + self.delay;
        self.host_seen = resume_from + self.delay;

        if new_host == self.my_pid {
            self.kind = SessionKind::Host;
            for &b in &bots {
                self.bot_q.entry(b).or_default();
            }
            self.status = format!("you now host the world — resumed at tick {resume_from}");
        } else {
            self.status = format!("player {new_host} now hosts — world resumed at tick {resume_from}");
        }
        self.host_gone = false;
        self.migration = None;
    }

    /// Host: the world has reached the freeze point — let the lobby in.
    fn try_admit(&mut self) {
        let Some(fa) = self.freeze_at else { return };
        if self.world.tick < fa {
            return;
        }
        let snapshot = self.world.save_bytes();
        while let Some((conn, key, name, color, listen_port)) = self.lobby.pop_front() {
            let Some(pid) = Session::pid_for(&self.world, &self.roster, &name, &key) else {
                self.send_to(conn, &Msg::Deny { reason: "world is full".into() });
                continue;
            };
            let ip = self.transport.remote_ip(conn);
            let info = PeerInfo {
                pid,
                name: name.clone(),
                color,
                addr: format!("{ip}:{listen_port}"),
                key,
                bot: false,
            };
            let peers: Vec<PeerInfo> = self.roster.values().cloned().collect();
            self.send_to(conn, &Msg::Welcome { pid, start_tick: fa, peers, snapshot: snapshot.clone() });
            self.bind(conn, pid);
            self.broadcast(&Msg::PeerJoined { info: info.clone(), start_tick: fa }, Some(pid));
            self.apply_join(info, fa);
        }
    }

    /// Client: a host that has gone silent (frozen/crashed without dropping
    /// the link) must also trigger migration, not just a clean socket close.
    fn host_watchdog(&mut self) {
        if self.migration.is_some() {
            return;
        }
        let silent = self
            .last_heard
            .get(&self.host_pid)
            .map(|t| self.now - *t > PEER_TIMEOUT_S)
            .unwrap_or(false);
        if silent {
            self.status = "host went silent — migrating…".into();
            self.begin_migration();
        }
    }

    /// Host: liveness watchdog over the transport's wall clock.
    fn watchdog(&mut self) {
        let dead: Vec<u8> = self
            .roster
            .keys()
            .copied()
            .filter(|p| *p != self.my_pid && !self.bot_q.contains_key(p))
            .filter(|p| {
                self.last_heard
                    .get(p)
                    .map(|t| self.now - *t > PEER_TIMEOUT_S)
                    .unwrap_or(false)
            })
            .collect();
        for p in dead {
            self.status = format!("player {p} timed out");
            if let Some(conn) = self.conn_of.remove(&p) {
                self.pid_of.remove(&conn);
                self.conn_key.remove(&conn);
                self.transport.close(conn);
            }
            self.handle_down(p);
        }
    }

    /// Graceful exit: close every link so peers see us go immediately
    /// (the host then broadcasts the canonical departure record).
    pub fn leave(&mut self) {
        let conns: Vec<ConnId> = self.conn_of.values().copied().collect();
        for conn in conns {
            self.transport.close(conn);
        }
        self.conn_of.clear();
        self.pid_of.clear();
        self.conn_key.clear();
    }

    /// For headless seed nodes: stay in the roster as the pacing host, but
    /// don't found a settlement (drops the auto-queued Join).
    pub fn skip_settle(&mut self) {
        self.local_q.retain(|c| !matches!(c, Command::Join { .. }));
    }
}

// ----------------------------------------------------------------------
// Joining, as a pollable state machine
// ----------------------------------------------------------------------

/// The join handshake without blocking: dial the host, say Hello, wait for
/// a verified Welcome. Pump `poll()` from your frame loop; it yields the
/// finished `Session` (or the denial). The native `Session::join` wraps
/// this in a sleep-loop; the browser drives it directly.
pub struct Joiner {
    transport: Option<Box<dyn Transport>>,
    host_conn: ConnId,
    identity: Option<Identity>,
    seq: u64,
    name: String,
    color: u8,
    hello_sent: bool,
    /// pin the host to a key known in advance (e.g. from a Nostr beacon);
    /// None = trust-on-first-use
    expect_host: Option<PubKey>,
}

impl Joiner {
    pub fn new(
        mut transport: Box<dyn Transport>,
        host_addr: &str,
        identity: Identity,
        name: &str,
        color: u8,
    ) -> Option<Joiner> {
        let host_conn = transport.dial(host_addr)?;
        Some(Joiner {
            transport: Some(transport),
            host_conn,
            identity: Some(identity),
            seq: crypto::unix_millis().wrapping_mul(1024),
            name: name.into(),
            color,
            hello_sent: false,
            expect_host: None,
        })
    }

    /// Require the Welcome to be signed by this exact key.
    pub fn expect_host_key(&mut self, key: PubKey) {
        self.expect_host = Some(key);
    }

    /// Returns Some(...) when the handshake resolves.
    pub fn poll(&mut self) -> Option<Result<Session, String>> {
        let t = self.transport.as_mut().expect("Joiner polled after completion");
        if !self.hello_sent {
            // the transport queues this until the dial completes
            let lp = t.listen_port();
            let id = self.identity.as_ref().unwrap();
            let hello = Msg::Hello { name: self.name.clone(), color: self.color, listen_port: lp };
            let bytes = Envelope::seal(id, self.seq, &hello);
            self.seq += 1;
            t.send(self.host_conn, &bytes);
            self.hello_sent = true;
        }
        let mut evs = t.poll().into_iter();
        while let Some(ev) = evs.next() {
            match ev {
                TransportEv::Data { conn, bytes } if conn == self.host_conn => {
                    let Ok(env) = Envelope::decode(&bytes) else { continue };
                    if !env.verify() {
                        continue;
                    }
                    if let Some(expected) = self.expect_host {
                        if env.sender != expected {
                            return Some(Err("host is not who the beacon claimed".into()));
                        }
                    }
                    let Ok(msg) = Msg::decode(&env.payload) else { continue };
                    match msg {
                        Msg::Welcome { pid, start_tick, peers, snapshot } => {
                            let transport = self.transport.take().unwrap();
                            let identity = self.identity.take().unwrap();
                            // the host resumes broadcasting right after
                            // Welcome; anything already behind it in this
                            // batch is real traffic for the new session
                            let leftover: Vec<TransportEv> = evs.collect();
                            return Some(Session::from_welcome(
                                transport, self.host_conn, identity, self.seq, env.sender, env.seq, pid,
                                start_tick, peers, snapshot, &self.name, self.color, leftover,
                            ));
                        }
                        Msg::Deny { reason } => return Some(Err(format!("denied: {reason}"))),
                        _ => {} // Freeze etc: expected pre-welcome chatter
                    }
                }
                TransportEv::Closed { conn } if conn == self.host_conn => {
                    return Some(Err("connection to host lost".into()));
                }
                _ => {}
            }
        }
        None
    }
}
