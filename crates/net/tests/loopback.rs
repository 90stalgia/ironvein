//! The networking proof: real TCP sockets on loopback, a world that starts
//! solo, gets joined mid-game, exchanges commands, stays hash-identical,
//! and survives a peer leaving.

use ironvein_net::{Identity, Session, SessionKind, TICK_DT};
use ironvein_sim::command::Command;
use ironvein_sim::mapgen;
use std::thread;
use std::time::Duration;

fn pump(s: &mut Session) -> u32 {
    s.update(TICK_DT)
}

#[test]
fn late_join_runs_in_lockstep_and_departure_is_clean() {
    let world = mapgen::verdant_divide(0xC0FFEE, ironvein_sim::world::Mode::Persistent);
    let port = 47361u16;
    let mut host = Session::host(world, Identity::generate(), port, "keeper", 0, &["Bot Gravel".to_string()])
        .expect("host bind");
    assert_eq!(host.kind, SessionKind::Host);

    // Let the host's world get some history before anyone joins.
    for _ in 0..40 {
        pump(&mut host);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(host.world.tick > 20, "host world should be advancing solo");

    // A settler knocks. join() blocks until the freeze+welcome dance is done,
    // so it runs on its own thread while we keep pumping the host.
    let addr = format!("127.0.0.1:{port}");
    let joiner = thread::spawn(move || Session::join(&addr, Identity::generate(), port + 1, "alice", 3));
    let mut alice = loop {
        pump(&mut host);
        thread::sleep(Duration::from_millis(2));
        if joiner.is_finished() {
            break joiner.join().unwrap().expect("alice joins");
        }
    };
    assert_eq!(alice.kind, SessionKind::Client);
    assert_ne!(alice.my_pid, host.my_pid);

    // Run both ends, with traffic flowing each way.
    let mut said_hello = false;
    for i in 0..900 {
        pump(&mut host);
        pump(&mut alice);
        if i == 60 {
            alice.queue(Command::Chat { text: "settling the east bank".into() });
            said_hello = true;
        }
        if i == 120 {
            host.queue(Command::Chat { text: "welcome, alice".into() });
        }
        thread::sleep(Duration::from_millis(1));
    }

    assert!(host.desync_at.is_none(), "host flagged desync: {}", host.status);
    assert!(alice.desync_at.is_none(), "alice flagged desync: {}", alice.status);
    assert!(alice.world.tick > 100, "alice's world should be running (tick {})", alice.world.tick);
    assert_eq!(host.peer_count(), 3, "keeper + bot + alice");
    assert_eq!(alice.peer_count(), 3);

    // Both worlds agree alice exists and her words made it into the record.
    let ap = alice.my_pid as usize;
    assert!(host.world.players[ap].joined, "host world never saw alice's Join");
    assert!(alice.world.players[ap].joined);
    assert!(said_hello && host.world.chat.iter().any(|c| c.2.contains("east bank")));
    assert!(alice.world.chat.iter().any(|c| c.2.contains("welcome")));

    // Settle to a common tick and compare full state hashes directly.
    for _ in 0..200 {
        pump(&mut host);
        pump(&mut alice);
        thread::sleep(Duration::from_millis(1));
        if host.world.tick == alice.world.tick && host.world.tick % 7 == 3 {
            assert_eq!(host.world.hash(), alice.world.hash(), "same tick, different worlds");
        }
    }

    // Alice quits. The host must notice, broadcast the verdict, and keep going.
    alice.leave();
    drop(alice);
    let before = host.world.tick;
    for _ in 0..300 {
        pump(&mut host);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(host.peer_count(), 2, "alice should be out of the roster");
    assert!(host.world.tick > before + 50, "world must keep flowing after a departure");
    assert!(host.world.players[ap].joined, "her settlement persists offline");
    assert!(host.desync_at.is_none());
}

#[test]
fn three_way_mesh_stays_identical() {
    let world = mapgen::verdant_divide(0xBEEF, ironvein_sim::world::Mode::Persistent);
    let port = 47391u16;
    let mut host = Session::host(world, Identity::generate(), port, "keeper", 0, &[]).expect("host bind");

    for _ in 0..20 {
        pump(&mut host);
        thread::sleep(Duration::from_millis(2));
    }

    let addr1 = format!("127.0.0.1:{port}");
    let j1 = thread::spawn(move || Session::join(&addr1, Identity::generate(), port + 1, "bea", 2));
    let mut bea = loop {
        pump(&mut host);
        thread::sleep(Duration::from_millis(2));
        if j1.is_finished() {
            break j1.join().unwrap().expect("bea joins");
        }
    };

    // run a little, then a third peer joins the live pair
    for _ in 0..120 {
        pump(&mut host);
        pump(&mut bea);
        thread::sleep(Duration::from_millis(1));
    }
    let addr2 = format!("127.0.0.1:{port}");
    let j2 = thread::spawn(move || Session::join(&addr2, Identity::generate(), port + 2, "cole", 5));
    let mut cole = loop {
        pump(&mut host);
        pump(&mut bea);
        thread::sleep(Duration::from_millis(2));
        if j2.is_finished() {
            break j2.join().unwrap().expect("cole joins");
        }
    };

    for i in 0..900 {
        pump(&mut host);
        pump(&mut bea);
        pump(&mut cole);
        if i == 50 {
            bea.queue(Command::Chat { text: "mesh check".into() });
            cole.queue(Command::GiveCredits { to: bea.my_pid, amount: 100 });
        }
        thread::sleep(Duration::from_millis(1));
    }

    for s in [&host, &bea, &cole] {
        assert!(s.desync_at.is_none(), "desync: {}", s.status);
        assert_eq!(s.peer_count(), 3);
    }
    assert!(cole.world.tick > 80, "cole's world is live");
    // direct state comparison whenever the three line up
    for _ in 0..200 {
        pump(&mut host);
        pump(&mut bea);
        pump(&mut cole);
        thread::sleep(Duration::from_millis(1));
        if host.world.tick == bea.world.tick && bea.world.tick == cole.world.tick {
            let h = host.world.hash();
            assert_eq!(h, bea.world.hash());
            assert_eq!(h, cole.world.hash());
        }
    }
}

#[test]
fn host_migration_keeps_the_world_alive() {
    // A host and two clients in a full mesh. The host's machine dies. The
    // survivors must freeze at the same tick, elect the lowest surviving pid,
    // and resume the *exact same world* with that peer now hosting.
    let world = mapgen::verdant_divide(0xD15EA5E, ironvein_sim::world::Mode::Persistent);
    let port = 47411u16;
    let mut host = Session::host(world, Identity::generate(), port, "keeper", 0, &[]).expect("host bind");

    for _ in 0..20 {
        pump(&mut host);
        thread::sleep(Duration::from_millis(2));
    }

    let addr1 = format!("127.0.0.1:{port}");
    let j1 = thread::spawn(move || Session::join(&addr1, Identity::generate(), port + 1, "bea", 2));
    let mut bea = loop {
        pump(&mut host);
        thread::sleep(Duration::from_millis(2));
        if j1.is_finished() {
            break j1.join().unwrap().expect("bea joins");
        }
    };
    let addr2 = format!("127.0.0.1:{port}");
    let j2 = thread::spawn(move || Session::join(&addr2, Identity::generate(), port + 2, "cole", 5));
    let mut cole = loop {
        pump(&mut host);
        pump(&mut bea);
        thread::sleep(Duration::from_millis(2));
        if j2.is_finished() {
            break j2.join().unwrap().expect("cole joins");
        }
    };

    // run the trio for a while so each has real history
    for _ in 0..200 {
        pump(&mut host);
        pump(&mut bea);
        pump(&mut cole);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(bea.peer_count(), 3);
    let bea_pid = bea.my_pid;
    let cole_pid = cole.my_pid;
    assert!(bea_pid != 0 && cole_pid != 0, "host is pid 0; clients are not");
    let new_host_pid = bea_pid.min(cole_pid);

    // THE HOST DIES. Close its sockets (a browser-tab-close / crash drops the
    // WebRTC links just the same), so the survivors' readers surface the loss.
    host.leave();
    drop(host);

    // Pump the two survivors until the migration completes and the world is
    // flowing again under the new host.
    let mut migrated = false;
    for _ in 0..4000 {
        pump(&mut bea);
        pump(&mut cole);
        thread::sleep(Duration::from_millis(1));
        // migration done when both shed the dead host and resumed stepping
        if bea.peer_count() == 2 && cole.peer_count() == 2 && bea.kind == SessionKind::Host {
            // give them a moment to step past the resume window
            migrated = true;
        }
        if migrated && bea.world.tick > cole.world.tick.min(bea.world.tick) + 30 {
            break;
        }
    }

    assert!(migrated, "migration never completed (bea status: {}, cole status: {})", bea.status, cole.status);
    assert!(bea.desync_at.is_none(), "bea desynced during migration: {}", bea.status);
    assert!(cole.desync_at.is_none(), "cole desynced during migration: {}", cole.status);

    // The lowest surviving pid is the new host; the other is still a client.
    assert_eq!(bea.my_pid.min(cole.my_pid), new_host_pid);
    assert_eq!(bea.kind, SessionKind::Host, "lowest pid should now host");
    assert_eq!(cole.kind, SessionKind::Client);
    assert_eq!(bea.peer_count(), 2, "the dead host is out of the roster");
    assert_eq!(cole.peer_count(), 2);

    // The world kept its identity: the keeper's settlement (pid 0) persists.
    assert!(bea.world.players[0].joined, "the departed host's base persists");

    // And crucially: the two survivors are running the SAME world.
    let before = bea.world.tick;
    for _ in 0..400 {
        pump(&mut bea);
        pump(&mut cole);
        thread::sleep(Duration::from_millis(1));
        if bea.world.tick == cole.world.tick {
            assert_eq!(bea.world.hash(), cole.world.hash(), "survivors diverged after migration");
        }
    }
    assert!(bea.world.tick > before + 50, "world must keep flowing after migration");
}
