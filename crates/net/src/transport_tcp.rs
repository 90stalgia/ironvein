//! transport_tcp.rs — the native transport: a full TCP mesh.
//!
//! One reader thread per link decodes frames into an mpsc channel that
//! `poll()` drains on the caller's thread. Writes go straight onto the
//! socket under a mutex (or into a queue while a dial is still in
//! flight). All the threads live here; the session above is sans-io.

use crate::protocol::read_frame;
use crate::transport::{ConnId, Transport, TransportEv};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

enum Raw {
    /// An inbound link was accepted (already attached + reading).
    Accepted { conn: ConnId, link: Arc<Link> },
    /// A dialed link finished connecting.
    DialUp { conn: ConnId },
    Data { conn: ConnId, bytes: Vec<u8> },
    Down { conn: ConnId },
}

struct LinkState {
    /// None while a dial is in flight.
    stream: Option<TcpStream>,
    /// Frames queued before the dial completed.
    queue: Vec<Vec<u8>>,
    dead: bool,
}

/// One TCP link, shared between the writer (session thread) and its
/// reader/dialer threads.
struct Link {
    state: Mutex<LinkState>,
}

impl Link {
    fn new(stream: Option<TcpStream>) -> Arc<Link> {
        Arc::new(Link { state: Mutex::new(LinkState { stream, queue: Vec::new(), dead: false }) })
    }

    fn send_payload(&self, payload: &[u8]) {
        let Ok(mut st) = self.state.lock() else { return };
        if st.dead {
            return;
        }
        match &mut st.stream {
            Some(s) => {
                let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
                let len = (payload.len() as u32).to_le_bytes();
                if s.write_all(&len).and_then(|_| s.write_all(payload)).is_err() {
                    st.dead = true;
                }
            }
            None => st.queue.push(payload.to_vec()),
        }
    }

    /// Dial completed: attach the socket and flush everything queued.
    fn attach(&self, stream: TcpStream) {
        let Ok(mut st) = self.state.lock() else { return };
        let queued = std::mem::take(&mut st.queue);
        st.stream = Some(stream);
        for payload in queued {
            let s = st.stream.as_mut().unwrap();
            let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
            let len = (payload.len() as u32).to_le_bytes();
            if s.write_all(&len).and_then(|_| s.write_all(&payload)).is_err() {
                st.dead = true;
                break;
            }
        }
    }

    fn shutdown(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.dead = true;
            if let Some(s) = &st.stream {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

fn spawn_reader(stream: TcpStream, conn: ConnId, tx: Sender<Raw>) {
    thread::spawn(move || {
        let mut s = stream;
        loop {
            match read_frame(&mut s) {
                Ok(bytes) => {
                    if tx.send(Raw::Data { conn, bytes }).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = tx.send(Raw::Down { conn });
                    break;
                }
            }
        }
    });
}

pub struct TcpMesh {
    rx: Receiver<Raw>,
    tx: Sender<Raw>,
    conns: BTreeMap<ConnId, Arc<Link>>,
    next_id: Arc<AtomicU64>,
    listen_port: u16,
    epoch: std::time::Instant,
}

impl TcpMesh {
    /// Bind a listener (port 0 = OS-assigned) and start accepting.
    pub fn listen(port: u16) -> io::Result<TcpMesh> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        let listen_port = listener.local_addr()?.port();
        let (tx, rx) = channel();
        let next_id = Arc::new(AtomicU64::new(1));
        let atx = tx.clone();
        let aid = next_id.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_nodelay(true);
                let conn = aid.fetch_add(1, Ordering::Relaxed);
                let link = Link::new(Some(stream.try_clone().expect("clone tcp stream")));
                if atx.send(Raw::Accepted { conn, link }).is_err() {
                    break; // mesh dropped; stop accepting
                }
                spawn_reader(stream, conn, atx.clone());
            }
        });
        Ok(TcpMesh { rx, tx, conns: BTreeMap::new(), next_id, listen_port, epoch: std::time::Instant::now() })
    }
}

impl Transport for TcpMesh {
    fn poll(&mut self) -> Vec<TransportEv> {
        let mut out = Vec::new();
        while let Ok(raw) = self.rx.try_recv() {
            match raw {
                Raw::Accepted { conn, link } => {
                    self.conns.insert(conn, link);
                    out.push(TransportEv::Connected { conn });
                }
                Raw::DialUp { conn } => out.push(TransportEv::Connected { conn }),
                Raw::Data { conn, bytes } => out.push(TransportEv::Data { conn, bytes }),
                Raw::Down { conn } => {
                    self.conns.remove(&conn);
                    out.push(TransportEv::Closed { conn });
                }
            }
        }
        out
    }

    fn send(&mut self, conn: ConnId, bytes: &[u8]) {
        if let Some(link) = self.conns.get(&conn) {
            link.send_payload(bytes);
        }
    }

    fn dial(&mut self, addr: &str) -> Option<ConnId> {
        let conn = self.next_id.fetch_add(1, Ordering::Relaxed);
        let link = Link::new(None);
        self.conns.insert(conn, link.clone());
        let tx = self.tx.clone();
        let addr = addr.to_string();
        thread::spawn(move || match TcpStream::connect(&addr) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                let reader = stream.try_clone().expect("clone tcp stream");
                link.attach(stream);
                let _ = tx.send(Raw::DialUp { conn });
                spawn_reader(reader, conn, tx);
            }
            Err(_) => {
                let _ = tx.send(Raw::Down { conn });
            }
        });
        Some(conn)
    }

    fn close(&mut self, conn: ConnId) {
        if let Some(link) = self.conns.remove(&conn) {
            link.shutdown();
        }
    }

    fn remote_ip(&self, conn: ConnId) -> String {
        self.conns
            .get(&conn)
            .and_then(|l| l.state.lock().ok())
            .and_then(|st| st.stream.as_ref().and_then(|s| s.peer_addr().ok()))
            .map(|a| a.ip().to_string())
            .unwrap_or_default()
    }

    fn listen_port(&self) -> u16 {
        self.listen_port
    }

    fn now_s(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
}
