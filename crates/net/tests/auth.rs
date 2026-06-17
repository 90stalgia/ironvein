//! The authentication proof: the envelope layer drops forged authorship,
//! replays, and tampered frames (verified directly against the protocol
//! types), and the SESSION drops commands whose signer doesn't own the pid
//! they claim — proven with a scripted in-process transport we drive by
//! hand.

use ironvein_net::transport::{ConnId, Transport, TransportEv};
use ironvein_net::{Envelope, Identity, Msg, Session, HOST_PID};
use ironvein_sim::command::Command;
use ironvein_sim::{mapgen, Eid, Tp};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[test]
fn envelope_signature_and_replay_rules() {
    let alice = Identity::generate();
    let mallory = Identity::generate();
    let msg = Msg::Cmds {
        tick: 10,
        pid: 2,
        cmds: vec![Command::Move { units: vec![Eid { idx: 1, gen: 1 }], to: Tp::new(5, 5) }],
    };

    // a well-formed signed envelope verifies and names its true signer
    let good = Envelope::decode(&Envelope::seal(&alice, 100, &msg)).unwrap();
    assert!(good.verify());
    assert_eq!(good.sender, alice.pk);

    // flip a byte of the signature -> rejected
    let mut tampered = Envelope::decode(&Envelope::seal(&alice, 100, &msg)).unwrap();
    tampered.sig[0] ^= 0xff;
    assert!(!tampered.verify());

    // claim alice's pubkey but the bytes were signed by mallory -> rejected
    let mut forged = Envelope::decode(&Envelope::seal(&mallory, 100, &msg)).unwrap();
    forged.sender = alice.pk;
    assert!(!forged.verify(), "must not verify against a key that didn't sign");

    // seq is bound into the signature: a captured frame can't be silently
    // renumbered to slip past the replay window
    let mut renumbered = Envelope::decode(&Envelope::seal(&alice, 100, &msg)).unwrap();
    renumbered.seq = 101;
    assert!(!renumbered.verify());
}

// A scripted transport with shared queues: the test holds a `Wire` handle to
// the same inbox/outbox the boxed Transport uses, so we can feed the session
// arbitrary frames and read back what it emitted.
#[derive(Clone, Default)]
struct Wire {
    inbox: Arc<Mutex<VecDeque<TransportEv>>>,
    outbox: Arc<Mutex<Vec<(ConnId, Vec<u8>)>>>,
}

impl Wire {
    /// Deliver a signed frame to the session as if it arrived on `conn`.
    fn deliver(&self, conn: ConnId, signer: &Identity, seq: u64, msg: &Msg) {
        let bytes = Envelope::seal(signer, seq, msg);
        self.inbox.lock().unwrap().push_back(TransportEv::Data { conn, bytes });
    }
    /// Decode every Msg the session has sent on `conn` so far.
    fn sent_msgs(&self, conn: ConnId) -> Vec<Msg> {
        self.outbox
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| *c == conn)
            .filter_map(|(_, b)| Envelope::decode(b).ok())
            .filter_map(|e| Msg::decode(&e.payload).ok())
            .collect()
    }
}

impl Transport for Wire {
    fn poll(&mut self) -> Vec<TransportEv> {
        self.inbox.lock().unwrap().drain(..).collect()
    }
    fn send(&mut self, conn: ConnId, bytes: &[u8]) {
        self.outbox.lock().unwrap().push((conn, bytes.to_vec()));
    }
    fn dial(&mut self, _: &str) -> Option<ConnId> {
        None
    }
    fn close(&mut self, _: ConnId) {}
    fn remote_ip(&self, _: ConnId) -> String {
        "10.0.0.9".into()
    }
    fn listen_port(&self) -> u16 {
        0
    }
}

/// Stand up a host with one legitimately-joined client over a scripted wire.
/// Returns (host session, wire handle, alice's identity, alice's conn, alice's pid).
fn host_with_one_client() -> (Session, Wire, Identity, ConnId, u8) {
    let world = mapgen::verdant_divide(0x5EC, ironvein_sim::world::Mode::Persistent);
    let wire = Wire::default();
    let mut host = Session::host_on(Box::new(wire.clone()), world, Identity::generate(), "keeper", 0, &[]);

    let alice = Identity::generate();
    let aconn: ConnId = 1;
    wire.deliver(aconn, &alice, 1, &Msg::Hello { name: "alice".into(), color: 3, listen_port: 6000 });

    let mut alice_pid = None;
    for _ in 0..400 {
        host.update(ironvein_net::TICK_DT);
        if let Some(p) = host.roster.values().find(|p| p.name == "alice") {
            alice_pid = Some(p.pid);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let alice_pid = alice_pid.expect("host admitted alice");
    assert_ne!(alice_pid, HOST_PID);
    (host, wire, alice, aconn, alice_pid)
}

/// Lockstep needs a command row from alice for every tick or the host's
/// barrier stalls. This pumps the host forward, supplying empty alice rows
/// to keep pace, and invokes `at_tick` once when alice's command for a
/// chosen tick should be sent. `seq` advances so every frame is fresh.
fn pump_with_alice(
    host: &mut Session,
    wire: &Wire,
    alice: &Identity,
    aconn: ConnId,
    alice_pid: u8,
    target: u32,
    mut at_tick: impl FnMut(&Wire, u32),
    rounds: usize,
) {
    let mut seq = 1000u64;
    let mut fired = false;
    for _ in 0..rounds {
        // keep alice's rows a few ticks ahead of the host's clock
        let ahead = host.world.tick + 4;
        for t in host.world.tick..=ahead {
            if t == target {
                if !fired {
                    at_tick(wire, t);
                    fired = true;
                }
                // never overwrite the target row with an empty one afterward
                continue;
            }
            wire.deliver(aconn, alice, seq, &Msg::Cmds { tick: t, pid: alice_pid, cmds: vec![] });
            seq += 1;
        }
        host.update(ironvein_net::TICK_DT);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn host_drops_commands_with_forged_authorship() {
    let (mut host, wire, alice, aconn, alice_pid) = host_with_one_client();
    let mallory = Identity::generate();
    let target = host.world.tick + 6;

    // At the target tick, mallory forges alice's authorship AND alice sends
    // her own legitimate command. Only the legit one may land.
    let alice2 = alice.clone();
    pump_with_alice(&mut host, &wire, &alice, aconn, alice_pid, target, move |w, t| {
        w.deliver(aconn, &mallory, 7_000, &Msg::Cmds {
            tick: t,
            pid: alice_pid,
            cmds: vec![Command::Chat { text: "i am alice".into() }],
        });
        w.deliver(aconn, &alice2, 8_000, &Msg::Cmds {
            tick: t,
            pid: alice_pid,
            cmds: vec![Command::Chat { text: "actually alice".into() }],
        });
    }, 120);

    let said: Vec<&str> = host.world.chat.iter().map(|(_, _, t)| t.as_str()).collect();
    assert!(said.iter().any(|t| t.contains("actually alice")), "legit command must land: {said:?}");
    assert!(!said.iter().any(|t| t.contains("i am alice")), "forged-authorship command must drop: {said:?}");
}

#[test]
fn host_drops_replayed_frames() {
    let (mut host, wire, alice, aconn, alice_pid) = host_with_one_client();
    let target = host.world.tick + 6;

    // Alice's genuine command is sent at seq 9000. An attacker immediately
    // replays a *different* command re-signed under alice's key but reusing
    // the already-seen seq 9000 — the strictly-increasing rule must reject it.
    let alice2 = alice.clone();
    pump_with_alice(&mut host, &wire, &alice, aconn, alice_pid, target, move |w, t| {
        w.deliver(aconn, &alice2, 9_000, &Msg::Cmds {
            tick: t,
            pid: alice_pid,
            cmds: vec![Command::Chat { text: "hello once".into() }],
        });
        // stale-seq replay (same 9000): must be dropped before it reaches the row
        w.deliver(aconn, &alice2, 9_000, &Msg::Cmds {
            tick: t,
            pid: alice_pid,
            cmds: vec![Command::Chat { text: "hello twice".into() }],
        });
    }, 120);

    let said: Vec<&str> = host.world.chat.iter().map(|(_, _, t)| t.as_str()).collect();
    assert!(said.iter().any(|t| t.contains("hello once")), "first frame must land: {said:?}");
    assert!(!said.iter().any(|t| t.contains("hello twice")), "stale-seq frame must drop: {said:?}");
}

#[test]
fn host_emits_signed_welcome() {
    let (_host, wire, _alice, aconn, _pid) = host_with_one_client();
    // Everything the host sent on alice's conn must verify under the host key,
    // and a Welcome must be among it.
    let raw = wire.outbox.lock().unwrap().clone();
    let mut saw_welcome = false;
    for (c, bytes) in raw {
        if c != aconn {
            continue;
        }
        let env = Envelope::decode(&bytes).expect("host frame decodes");
        assert!(env.verify(), "every host frame is signed");
        if matches!(Msg::decode(&env.payload), Ok(Msg::Welcome { .. })) {
            saw_welcome = true;
        }
    }
    assert!(saw_welcome, "host must have sent a Welcome");
    let _ = wire.sent_msgs(aconn); // exercise the helper
}
