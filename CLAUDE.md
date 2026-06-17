# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

IRONVEIN — a peer-to-peer persistent-world RTS (Westwood-style) in a single Rust workspace. No game assets (all sprites procedural), no external servers. See `README.md` for gameplay and `ARCHITECTURE.md` for the full engineering story; the latter is the authoritative reference for the lockstep/join/leave protocol.

## Commands

```sh
cargo build --release          # needs desktop GL deps: libx11-dev libxi-dev libgl1-mesa-dev libasound2-dev
cargo test --release           # full suite: determinism, signed-auth, live TCP loopback, host migration
cargo test -p ironvein-sim --test determinism            # determinism suite only
cargo test -p ironvein-net --test loopback               # live TCP join/leave/mesh + host-migration
cargo test -p ironvein-net --test auth                   # BIP-340 envelope auth: forgery/replay drops
cargo test -p ironvein-net <name>                         # single test (crypto/nostr/signaling unit tests too)

# WebAssembly / browser build (serverless: WebRTC + Nostr)
cargo build -p ironvein --target wasm32-unknown-unknown --release   # emits ironvein.wasm
# then serve index.html (loads miniquad gl.js, js/ironvein_net.js, the wasm)

./target/release/ironvein                                # play (one bot neighbor)
./target/release/ironvein --host 47777 --name Ada        # native TCP host
./target/release/ironvein --join HOST:47777 --name Bo    # native TCP join live
./target/release/ironvein --demo --bots 2 --map skirmish # unattended observer match, writes PNGs to shots/
./target/release/ironvein-seed --port 47777 --bots 1     # headless world keeper, autosaves saves/world.iv
```

The `--demo` mode works under `xvfb-run` for headless verification/screenshots.
When changing `crates/net`, build BOTH targets — native and `wasm32-unknown-unknown` —
since the WebRTC transport, crypto, and Nostr layers are wasm-gated.

## Workspace layout

- `crates/sim` — deterministic simulation core (**zero deps, must stay that way**). The whole game lives here: `world.rs` is `World::step` and the tick pipeline; also entities, A* pathfinding, mapgen, bot AI, hand-rolled serializer. `Command::Join` and `Player` carry a 32-byte identity key (opaque to the sim; save format v2).
- `crates/net` — sans-io lockstep `Session` plus a pluggable `Transport`:
  - `session.rs` — the engine room: freeze-join, host-arbitrated departure, hash gossip, **host migration**. Owns no sockets/threads; consumes `TransportEv`, emits frames.
  - `transport.rs` — the `Transport`/`ConnId`/`TransportEv` seam; `transport_tcp.rs` (native TCP mesh, reader threads), `transport_webrtc.rs` (wasm32, WebRTC via JS FFI).
  - `protocol.rs` — wire format; every frame is a signed `Envelope` wrapping a `Msg`.
  - `crypto.rs` — secp256k1 identity, BIP-340 signing/verify, ECDH + ChaCha20-Poly1305.
  - `nostr.rs` / `signaling.rs` — NIP-01 events (beacons kind 29001, encrypted signaling kind 29000), `RelayClient` + `MockRelay` + matchmaking `Lobby`.
  - `browser.rs` (wasm32) — `Matchmaker` tying Lobby + WebRTC signaling for the browser frame loop.
- `crates/client` — macroquad client (`ironvein` binary). Native: TCP host/join/solo. wasm32: WebRTC host + Nostr beacon, driven by `browser::Matchmaker`. All art procedural; floats allowed here, never in sim.
- `crates/seed` — headless host (`ironvein-seed` binary) that paces the clock, runs bots, autosaves.

Dependency direction: client/seed → net → sim. The wasm-only deps (`getrandom/custom`, JS FFI) are `cfg(target_arch = "wasm32")`-gated; native and wasm builds must both stay clean.

## The serverless net stack (ARCHITECTURE.md §8 is authoritative)

Four pieces, all behind the unchanged sans-io `Session`:

1. **Signed commands** — every wire frame is an `Envelope { sender, seq, payload, sig }` (BIP-340 Schnorr). The session drops anything failing: bad sig, non-increasing `seq` (replay), wrong key-for-link, or `Cmds{pid}` whose signer ≠ `roster[pid].key`. Host-control verdicts (`Freeze`/`PeerJoined`/`Left`/`MigrateResume`) are honored only from the host key. **A settler's identity key is its Nostr key** — one keypair for both.
2. **Nostr matchmaking** — hosts publish kind-29001 region beacons (`["t","ironvein-region-XX"]`); joiners subscribe, pick a host pubkey, and trade ECDH-encrypted SDP/ICE as kind-29000 events. Relays are untrusted: events are verified, payloads encrypted, the host key pinned (`Joiner::expect_host_key`).
3. **WebRTC transport** — once the data channel opens, Nostr is done; gameplay is pure P2P. `js/ironvein_net.js` owns the sockets/peer-connections (no keys, no logic); Rust owns all crypto and the sim.
4. **Host migration** — host loss freezes everyone at the same tick (the barrier); lowest surviving non-bot pid is elected deterministically and ships its frozen world as a `MigrateResume{snapshot}` (snapshot transfer, NOT command-replay — replay breaks across mid-stream joins). Survivors load identical bytes, re-key bots, resume. Tested by `host_migration_keeps_the_world_alive`.

## The determinism contract (read before touching `crates/sim`)

Multiplayer works only because every peer computes a bit-identical world from (seed + command stream). The hard rules, stated in `crates/sim/src/lib.rs`:

1. **No floats in `sim`.** Positions are `i32` fixed point, `FX = 256` sub-units per tile. Use `isqrt`/`step_toward`/`dir16` style integer math.
2. **No iteration over unordered containers.** Entities live in a generational arena (`Vec<Option<Ent>>`) walked by index, free slots reused lowest-index-first; sim-state maps are `BTreeMap`.
3. **One RNG** (`World.rng`, PCG32, part of the save). Nothing else rolls dice. `Date`-like or thread-dependent inputs are forbidden.
4. **`World::step(commands)` is the only mutator.** Commands apply sorted by player id, then submission order.
5. **`save_bytes()` must be byte-identical across peers**, and `hash()` (FNV-64) must cover any field you add to state. Peers cross-check hashes every 32 ticks; a missed field means silent desync.

The tick pipeline order in `world.rs` (commands → construction → production → harvesting → movement → combat → ore regrowth → income/healing → power/defeat → fog) is part of the contract — reordering it is a save-breaking, desync-causing change. Likewise, adding/removing serialized fields breaks existing `saves/world.iv` files.

The determinism tests are the enforcement: bot wars run twice and compared byte-for-byte every tick, plus save-at-tick-600/reload/continue-on-same-hash. If you change sim behavior, these tests *should* still pass (determinism ≠ unchanged behavior); if they fail, you broke a rule above.

## Networking model (for `crates/net` work)

- Lockstep at `TICK_HZ = 10` with `DELAY = 3` ticks input delay; a tick executes only when commands from the whole roster arrived. Hash check every `HASH_EVERY = 32` ticks.
- Clients pace sends off the host's command stream — this invariant is what makes freeze-join correct (no peer can issue a command past the freeze tick). Don't "optimize" it away.
- Bots run only on the host; their commands replicate like a player's, which is how they stay deterministic.
- Host's record is canon for departures (`Left{pid, from, backfill}`); leavers' bases persist and their pid is reserved by name for rejoin.
- `MAX_PLAYERS = 8` is an architectural ceiling (everyone simulates everything), not a UI choice.
