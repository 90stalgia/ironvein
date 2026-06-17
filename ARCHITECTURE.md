# IRONVEIN — architecture

This document is the engineering story: how a persistent RTS world runs
on nothing but its players' machines, why it can't drift apart, and where
the honest limits are.

## 1. The one big idea: deterministic lockstep

Peers never exchange game state. They exchange **commands** ("player 2:
build Barracks at (41,37)"), apply them on the same tick, and run the
identical simulation. If the sim is a pure function of (initial state,
command stream), every machine computes the same world forever. That's
what makes P2P viable: bandwidth is a few commands per second regardless
of army size, and a "server" is just whoever paces the clock.

The price is a **determinism contract**, enforced in `crates/sim`:

1. **No floats in the sim.** All positions/velocities are `i32` fixed
   point (`FX = 256` sub-units per tile). Floats are for the renderer.
2. **No iteration over unordered containers.** Entities live in a
   generational arena (`Vec<Option<Ent>>`) walked by index; every map in
   simulation state is a `BTreeMap`. Free slots are reused
   lowest-index-first so spawn order is reproducible.
3. **One RNG, owned by the world.** A PCG32 seeded at mapgen; nothing
   else may roll dice. The RNG state is part of the save.
4. **`World::step(commands)` is the only mutator**, and commands are
   applied sorted by player id, then in submission order.
5. **The whole state serializes byte-identically** (`save_bytes`), and
   `hash()` folds every field that matters through FNV-64. If two peers
   ever disagree, the hash says so within 32 ticks.

The integration tests treat the contract as law: two full bot wars are
run twice and compared **byte-for-byte every tick**; a war is saved at
tick 600, reloaded, and must continue on the exact same hash sequence.

## 2. Time

The sim runs at **10 Hz** (`TICK_HZ`). Commands issued at tick *T* are
scheduled for *T + 3* (the input delay), giving the network 300 ms to
deliver them — the classic RTS trick that hides latency entirely as long
as the link is faster than that. The renderer interpolates entity
positions between the last two ticks, so 10 Hz simulation still looks
like 60 fps motion.

A tick executes a fixed pipeline (commands → construction → production →
harvesting → movement & separation → combat & capture → ore regrowth →
farm income / house healing → power & defeat bookkeeping → fog). Order is
part of the contract; touching it is a save-breaking change.

## 3. The wire

Plain TCP, full mesh, length-prefixed frames, a hand-rolled serializer
(`crates/net/protocol.rs`). No TLS, no NAT traversal — LAN, VPN
(Tailscale/WireGuard), or a port-forward. That's an honest scope choice
for a PoC; the production path is WebRTC data channels or QUIC, which
slot in behind the same `Session` API (everything above the socket is
transport-agnostic).

Messages: `Hello, Welcome, Deny, Freeze, PeerJoined, Dial, Cmds,
HashChk, Left`.

### Steady state

Every peer sends `Cmds{tick, pid, cmds}` for each tick — empty most of
the time — up to 3 ticks ahead. A tick executes only when commands from
**everyone in the roster** have arrived (lockstep barrier). The host
additionally ships commands for the bots it drives; bot brains run only
on the host, but their *commands* replicate like anyone else's, so bots
are deterministic for free.

Clients pace their own sends off the host's stream (never send tick *T*
until the host's *T* arrived). This costs nothing in steady state — the
host is always ahead — and is what makes joining airtight, below.

Every 32 ticks each peer broadcasts `HashChk{tick, hash}`. A mismatch
halts the local world with a red banner rather than letting two realities
diverge quietly. (In ~10⁵ ticks of automated war, zero mismatches.)

### Joining a live world — the freeze dance

The hard problem: a snapshot is only valid for a tick nobody has issued
commands past.

1. Joiner connects to the host, sends `Hello{name, color, listen_port}`.
2. Host picks a freeze point `fa = now + 3·delay + 3`, broadcasts
   `Freeze{fa}` (informational) and **stops sending its own commands at
   `fa`**. Because clients pace off the host, no peer can ever send a
   command for tick ≥ `fa`. No acks needed; TCP FIFO does the rest.
3. Every world stalls at `fa` (the barrier can't be satisfied past it).
4. Host snapshots (`save_bytes`, byte-identical by contract), assigns a
   pid — **rejoining names reclaim their old pid and base** — and sends
   `Welcome{pid, start_tick: fa, peers, snapshot}` to the joiner and
   `PeerJoined{info, fa}` to everyone else.
5. Everyone (joiner included) force-seeds **empty** command rows for all
   pids for ticks `[fa, fa+delay)` and resumes with `next_send = fa +
   delay`. The window is empty *by definition*, so all peers agree
   byte-for-byte no matter what was in flight.
6. The joiner meshes out: dials every peer address from `Welcome`,
   identifying itself with `Dial{pid}`.

The new settler's first command is their own `Join`, which the sim
answers with a starter kit at a free spawn site. Multiple joiners in the
same freeze are admitted in sequence at the same `fa`.

### Leaving — the host's record is canon

TCP links die at different times on different peers, so peers may hold
*different* amounts of the leaver's final commands. Someone must decide
history; the host does. On detecting a drop (socket error, or a 30 s
liveness watchdog), the host finds the first tick it lacks from the
leaver and broadcasts `Left{pid, from, backfill}` — the leaver's last
known commands. Everyone executes the backfill (the departed peer
lingers as a "ghost" until `from`), then drops them from the barrier.
**Their entities are untouched**: bases persist offline, harvesters park,
towers keep shooting. The pid is reserved for that name to reclaim.

If the **host** dies, clients pause the world rather than elect a new
arbiter (host migration is the known missing piece — see §6). Nothing is
lost: every client has a current autosave and can re-host from it.

## 4. Persistence — the Second Life part

A "world" is just `save_bytes()`. Because every peer's bytes are
identical, persistence is radically simple:

- every peer autosaves `saves/world.iv` every 600 ticks (atomic
  write-rename);
- `ironvein-seed` is a 150-line headless peer that hosts, paces, runs
  optional bots, and autosaves — run it on a Pi and the valley never
  sleeps;
- if the seed box burns down, **anyone** runs
  `ironvein --load saves/world.iv --host 47777` and the world continues
  from at most a minute ago;
- offline players' settlements persist, can be raided, captured, or
  walled in; rejoining by name puts you back in your seat.

Persistent mode also softens the rules: defeat doesn't eliminate you
(you can rebuild from an engineer-captured village house), farms and
houses make a peaceful economy viable, and ore regrowth means the map
doesn't exhaust.

## 5. The client

`macroquad` for windowing/GL; everything else is hand-rolled. All art is
procedural and rendered in an **isometric 2.5D** projection (2:1 dimetric,
64×32 tiles): the square sim grid is projected to screen, terrain is a
back-to-front field of shaded diamonds, buildings are extruded boxes with a
lit roof and two shaded wall faces, units are billboards with cast shadows,
and everything is painter-sorted by tile depth. Picking inverts the
projection (screen→tile) for orders and placement. Fog is two darkness
levels, with a day/night tint on a 10-minute cycle. The
minimap renders into a CPU image refreshed every 12 frames and blitted
nearest-neighbor. The sidebar is the classic command bar: tabs, costs,
queue progress, power bar, low-power flash. Interpolation makes 10 Hz
feel smooth; a `--demo` mode plays an observer match unattended and
writes PNG screenshots with a built-in stored-deflate encoder (no image
crate), which is how the README shots were taken on a headless box.

## 6. Honest limits, and the path past them

**Lockstep carries ~8 players per world, comfortably.** Everyone
simulates everything, so world tick cost is the ceiling, and one slow
peer slows the tick for all (input delay hides ≤300 ms; beyond that the
world visibly hiccups). That's why `MAX_PLAYERS = 8` — it's the truth of
the architecture, not a UI choice.

The documented road to "MMO": **federate**. A continent is a grid of
128×128 regions, each its own lockstep mesh of the ≤8 players present,
each kept alive by its own seed process. Crossing a border is a
`Welcome` handshake with the neighbor mesh; trade between regions is a
signed command relayed by seeds. Nothing in `sim` or `session` changes —
regions are just worlds. (Deliberately out of PoC scope, but the
join/leave machinery above is exactly the hard part of it, built and
tested.)

**Cheating**: lockstep means every client holds full state, so maphacks
are physically possible — true of every lockstep RTS ever shipped
(StarCraft included). What P2P must add is *authorship* — and this is now
**built** (see §8): every settler is a secp256k1 keypair, every frame is a
BIP-340-signed envelope, and the engine drops any command whose signer
doesn't hold the pid it commands. Speed/economy hacks are already
impossible: an illegal command is rejected identically by every honest
sim, and a modified sim desyncs you out of the world within 32 ticks.

**Transport**: the lockstep core is now sans-io behind a `Transport`
trait. Native runs the original TCP mesh; the browser runs a WebRTC
data-channel mesh signaled over Nostr relays — fully serverless, with
host migration so a world never dies (see §8).

## 7. Why Rust

A lockstep game dies by the thousand-cut bugs other languages tolerate:
iterator invalidation mid-tick, accidental float creep, data races
between the render and net threads, integer UB. Rust turns each of those
into a compile error, `i32` overflow is defined (and checked in debug),
and the borrow checker is why the session pump / reader threads / render
loop share a world without a single unsafe block. Also: one static
binary per platform, which matters when your "server" is whoever's
laptop stayed on.

## 8. Serverless: Nostr signaling, WebRTC transport, signed commands, migration

The PoC ran on a TCP mesh with a port-forward. The production stance is
**no servers at all** — not for matchmaking, not for transport, not for
identity. Four pieces, each behind the same sans-io `Session`.

### 8.1 The transport seam (sans-io)

`Session` owns no sockets and spawns no threads. It consumes
`TransportEv {Connected, Data, Closed}` and emits raw frames through a
`Transport` trait (`crates/net/transport.rs`). Two implementations:

- `transport_tcp.rs` (native): the original full TCP mesh, now with
  reader threads behind the trait;
- `transport_webrtc.rs` (wasm32): a WebRTC data-channel mesh. The Rust
  side is ~150 lines of FFI; `js/ironvein_net.js` owns the actual
  `RTCPeerConnection`s and `RTCDataChannel`s. JS is a dumb pipe with a
  poll queue — it holds no keys and makes no decisions.

Because the session is identical above the seam, the join/leave/migration
logic, the hash gossip, and every test are transport-agnostic. The
session also keeps its own clock as accumulated `dt` and samples wall time
from the transport (`now_s`), since `std::time::Instant` doesn't exist on
wasm.

### 8.2 Identity & signed commands — `SignedNetMessage`

Every settler is a **secp256k1 keypair** (`crypto.rs`), minted on first
run and persisted (`saves/id-<name>.key`). This *is* the player's Nostr
key — one identity for signaling and for gameplay.

Every frame on the wire is an **`Envelope { sender, seq, payload, sig }`**
(`protocol.rs`): `payload` is an encoded `Msg`; `sig` is a BIP-340 Schnorr
signature over a tagged hash of `(sender ‖ seq ‖ payload)`. The session
runs a verification gauntlet before anything reaches the sim — drop on any
failure:

1. the signature must verify against the envelope's `sender`;
2. `seq` must be strictly increasing per sender (replay shield; seeded
   from wall-clock millis so a fresh session always outranks a replayed
   old one);
3. a link is locked to the first key that speaks on it;
4. **`Cmds{pid}` is honored only if `roster[pid].key == sender`** — you
   cannot move a unit you don't own, because you can't author a command
   for a pid whose key you don't hold (bots carry the driving host's key);
5. world-control verdicts (`Freeze`, `PeerJoined`, `Left`,
   `MigrateResume`) are honored only from the host's key.

The settler's key rides into the sim as 32 opaque bytes on `Command::Join`
and persists on the `Player` (`save` v2), so a base can only be reclaimed
by its keyholder, on any host, forever. A modified client that skips these
checks simply desyncs out within 32 ticks. (Tests: `auth.rs` —
forged-authorship dropped, replay dropped, every host frame signed.)

### 8.3 Matchmaking — Nostr region beacons & encrypted signaling

The "world map" is a grid of regions (`A1`, `B2`, …). Signaling rides two
Nostr event kinds (`nostr.rs`, `signaling.rs`):

- **kind 29001 — region beacon** (replaceable), tagged
  `["t","ironvein-region-<ID>"]`, content `{host pubkey, tick, players,
  genesis hash}`. A host republishes ~1/s; a newcomer subscribes to the
  topic and sees every live world and whom to dial. The genesis hash pins
  which world a region is, so a relay can't lure you onto a fork.
- **kind 29000 — signaling** (ephemeral), the WebRTC handshake: SDP
  offer/answer and ICE candidates, each **ECDH-encrypted** (secp256k1 →
  ChaCha20-Poly1305) to the recipient and tagged `["p",<recipient>]`.

Relays are untrusted plumbing: every event is Schnorr-verified, every
payload is end-to-end encrypted, and the joiner pins the host's key from
the beacon (`Joiner::expect_host_key`) so a relay can't substitute an
impostor `Welcome`. Once the data channel is up, **Nostr is never used
again** — gameplay flows peer-to-peer over WebRTC. The relay socket sits
behind a `RelayClient` trait: `MockRelay` (in-process, for tests),
`WasmRelay` (the JS `wss://` pool), or a native `ws://` client slotting in
the same way.

### 8.4 Host migration — the world outlives any host

If the host's link drops (or it goes silent past the watchdog), every
survivor **freezes at its current tick** — the lockstep barrier does this
for free: without the host's commands nobody can advance. Then:

1. **Deterministic election**: the lowest surviving non-bot pid becomes
   host. Every survivor computes the same answer from the same roster.
2. The new host declares **its own frozen world canonical** and ships it
   as a `MigrateResume{snapshot}` — exactly like a join `Welcome`. Loading
   identical bytes is bulletproof where replaying a command gap is not:
   survivors frozen at *different* ticks all converge byte-for-byte, with
   no roster-history or command-loss edge cases. (We tried command-replay
   first; a peer that joined mid-stream breaks it. Snapshot transfer is
   the robust answer, and it mirrors the existing "host's record is canon"
   principle.)
3. Everyone retires the dead host (its base persists in the bytes,
   reclaimable by its key), re-keys the bots to the new host, and resumes
   lockstep at the snapshot's tick behind an empty input window. The new
   host begins publishing the region's 29001 beacon.

The full mesh means survivors are already connected to each other, so no
re-dialing is needed. (Test: `host_migration_keeps_the_world_alive` —
kill the host mid-war; the lowest-pid survivor takes over and both peers
run the identical world, repeatably.)

### 8.5 Browser bring-up

`cargo build -p ironvein --target wasm32-unknown-unknown --release`
produces `ironvein.wasm`; serve `index.html` (which loads miniquad's
`gl.js`, then `js/ironvein_net.js`, then the wasm). The wasm client mints
an identity, hosts its region over WebRTC, and advertises on the relays;
`browser::Matchmaker::pump()` shuttles SDP/ICE across Nostr each frame
alongside `Session::update()`. k256's transitive `getrandom` is routed
through the JS `ivn_random` import (a custom source), so the whole crypto
stack runs in the browser without wasm-bindgen.

