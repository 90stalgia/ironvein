# IRONVEIN

A peer-to-peer persistent-world RTS in the old Westwood style —
isometric 2.5D, 64×32 tiles, ore trucks, tesla-less but proud. One Rust
workspace, zero game assets (every sprite is drawn procedurally — buildings
are extruded shaded boxes, units are billboards, terrain is a field of
diamonds — and every sound is synthesised in code, from the soundtrack to the
gunfire), zero external servers. The world keeps running while you sleep;
your base stands until someone with engineers says otherwise.

![IRONVEIN](media/ironvein_1.png)

## Play in the browser

No install — **https://90stalgia.github.io/ironvein/** runs the whole game as
WebAssembly. Single-player and skirmish-vs-bots are entirely client-side;
multiplayer is pure peer-to-peer over **WebRTC**, with colonies advertised and
the connection handshake carried over public **Nostr** relays. No matchmaking
server, no backend — the page is just static files. (Behind a strict/symmetric
NAT a direct link may not form; a LAN/VPN or the native build below always
connects.)

## What's in the box

- **A complete deterministic sim**: base building, power, ore economy with
  regrowing fields, infantry/vehicles, A* pathfinding with unit separation,
  fog of war, guard towers, walls, selling, capturing, rally points,
  a neutral village to absorb, farms and houses for the settler life,
  day/night tint, chat, and credit wiring between players.
- **Lockstep P2P networking** over plain TCP: host a world, friends join
  mid-game (the world pauses for a snapshot handshake, then flows on),
  leave cleanly, time out safely. State hashes are cross-checked every
  32 ticks so a desync halts loudly instead of drifting silently.
- **Persistence**: every peer autosaves the identical world bytes every
  minute. Anyone's save can re-host the world. A tiny headless **seed
  node** (`ironvein-seed`) keeps the valley alive 24/7 on any spare box.
- **Bots** that expand, harvest, build armies and raid across the river,
  in both skirmish and persistent modes.
- **One proof-of-concept map**: *Verdant Divide*, a 128×128 river valley
  with two bridges, six regrowing ore fields, rocky corners and a neutral
  village in the middle.

## Build & run

Needs only Rust (1.75+) and a desktop GL. On Debian/Ubuntu:

```
sudo apt install build-essential libx11-dev libxi-dev libgl1-mesa-dev libasound2-dev
cargo build --release
```

Then:

```
./target/release/ironvein                      # your own valley, one bot neighbor
./target/release/ironvein --bots 3 --map skirmish   # classic deathmatch
./target/release/ironvein --name Ada --color 2      # pick a callsign & color (0-7)
```

## Playing

Right panel has the minimap, money, power and the build tabs. Left-drag to
select, right-click to move / attack / harvest / set rally. `F1` in game
shows the full manual. The essentials:

| input            | action                                          |
|------------------|--------------------------------------------------|
| left drag        | select your units                                |
| right click      | move · attack · harvest · rally (context)        |
| `A` + click      | attack-move                                      |
| `S` / `X`        | stop · sell selected building                    |
| `Ctrl+1..9`, `1..9` | save / recall control group                  |
| `Enter`          | chat (`/give 2 500` wires 500 credits to player 2) |
| `F5`             | save world now (autosaves every minute anyway)   |
| `H`, arrows, screen edge | camera home · scroll                    |

Economy: harvesters chew glittering ore and bank it at a **Refinery**.
Ore regrows near its nodes, so fields are positions to hold, not puddles
to drain. **Farms** trickle credits, **Houses** raise your unit cap and
slowly heal infantry nearby. **Engineers** capture enemy or village
buildings (and are consumed). Low power makes everything build at half
speed — watch the bar.

## Multiplayer (the whole point)

Everything is peer-to-peer over TCP; there is no matchmaking server.
One peer **hosts**, everyone else **joins** and the peers form a full mesh.

```
# you
./target/release/ironvein --host 47777 --name Ada

# your friends (LAN, VPN/Tailscale, or a port-forward)
./target/release/ironvein --join 192.168.1.20:47777 --name Bo
```

Joining is live: the host freezes the world for a beat, snapshots it,
hands the newcomer a player slot and a starter kit (ConYard, harvester,
two riflemen) at a free spawn site, and the world resumes. Leaving (or
crashing — a 30 s watchdog catches half-dead links) is arbitrated by the
host so every survivor agrees on history. **Your base persists** after
you leave; rejoin with the same `--name` to reclaim it.

### Keeping a world alive forever

```
./target/release/ironvein-seed --port 47777 --bots 1
# later, or after a reboot:
./target/release/ironvein-seed --port 47777 --load saves/world.iv
```

The seed is a headless keeper: it hosts, paces the clock, runs bots if
asked, and autosaves `saves/world.iv` every minute. Because saves are
byte-identical on every peer, *any* player's autosave can resurrect the
world if the seed box dies: `ironvein --load saves/world.iv --host 47777`.

If the host vanishes mid-session, your client pauses the world (it can't
arbitrate alone) and your autosave is intact — relaunch from it.

## Tests

```
cargo test --release
```

The suite covers serialization round-trips, pathfinding, and the ones that
matter — full bot wars simulated twice byte-for-byte identically, save/load
mid-war continuing on the exact hash, a war that actually ends, **live TCP
loopback** tests (mid-game join, chat both ways, three-way mesh hash equality,
clean departure with the base left standing, host migration), and the
**signed-envelope auth** suite that drops forged or replayed frames.

## Layout

```
crates/sim     deterministic world: no floats, no HashMap iteration, one RNG
crates/net     lockstep sessions: freeze-join, departure canon, hash gossip
crates/client  macroquad client: procedural pixel art, sidebar, minimap
crates/seed    headless world keeper
```

`ARCHITECTURE.md` explains the determinism contract, the join/leave
protocol on the wire, the signed-envelope auth and the serverless
**WebRTC + Nostr** stack the browser build runs on, honest limits (lockstep
comfortably carries ~8 players per region), and the remaining path to a real
MMO (federated regions).

## Demo reel

`ironvein --demo --bots 2 --map skirmish` runs an unattended observer
match and drops three PNGs into `shots/` — that's how the screenshots in
this README were made (under `xvfb-run` on a headless box, even).

![IRONVEIN](media/ironvein_2.png)
![IRONVEIN](media/ironvein_3.png)
