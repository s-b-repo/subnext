//! Session sharding — concurrency across conversations, not within one.
//!
//! `Dcr` is a single-writer facade: one conversation, one runtime, no interior
//! locking. Conversations are naturally independent, so the honest concurrency
//! model is to shard by session and lock at the shard boundary, which gives
//! continuous-batching-style throughput with zero changes to the runtime's
//! internals. `serve` puts that behind a dependency-free TCP protocol so N
//! clients can share one process.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::authorization::{valid_label, PolicyEnforcementPoint, SessionPolicy};

use crate::runtime::Dcr;

/// One conversation = one runtime. Routing is by session id; eviction is LRU.
///
/// Eviction drops the runtime's whole state — session memory lives only here,
/// so `capacity` is a memory bound, and an evicted conversation restarts cold.
/// That is reported (`evicted`) rather than hidden, because a silently
/// forgotten conversation is the same failure as a stale fact. One exception
/// to the bound: a session mid-request is never evicted (that would split a
/// live conversation across two runtimes), so capacity is exceeded while
/// every slot is in flight.
pub struct SessionRouter {
    budget: usize,
    capacity: usize,
    sessions: HashMap<String, Arc<Mutex<Dcr>>>,
    ingested_bytes: HashMap<String, usize>,
    /// Oldest first, refreshed on touch.
    lru: VecDeque<String>,
    /// Sessions dropped to make room. A rising counter means capacity is too
    /// small for the working set, not that the router is misbehaving.
    pub evicted: u64,
}

impl SessionRouter {
    pub fn new(budget: usize, capacity: usize) -> Self {
        Self {
            budget,
            capacity: capacity.max(1),
            sessions: HashMap::new(),
            ingested_bytes: HashMap::new(),
            lru: VecDeque::new(),
            evicted: 0,
        }
    }

    pub fn route(&mut self, session: &str) -> Arc<Mutex<Dcr>> {
        if !self.sessions.contains_key(session) {
            if self.sessions.len() >= self.capacity {
                // Evict the least-recently-used session that is not in flight.
                // An entry another thread is mid-request on (strong_count > 1)
                // must not be dropped from the map: the next request for that
                // id would spawn a second, cold runtime and split the
                // conversation's state across two runtimes.
                let victim = self
                    .lru
                    .iter()
                    .position(|id| {
                        id != session
                            && self
                                .sessions
                                .get(id)
                                .is_some_and(|d| Arc::strong_count(d) == 1)
                    })
                    .map(|pos| self.lru.remove(pos).unwrap());
                if let Some(oldest) = victim {
                    self.sessions.remove(&oldest);
                    self.ingested_bytes.remove(&oldest);
                    self.evicted += 1;
                }
                // If every session is in flight, capacity is exceeded rather
                // than splitting a live conversation — a soft, reported-only
                // bound beats corrupting state.
            }
            self.sessions.insert(
                session.to_string(),
                Arc::new(Mutex::new(Dcr::new(self.budget))),
            );
        }
        self.lru.retain(|id| id != session);
        self.lru.push_back(session.to_string());
        self.sessions[session].clone()
    }

    fn route_request(&mut self, session: &str, bytes: usize) -> Option<Arc<Mutex<Dcr>>> {
        if !self.sessions.contains_key(session) && self.sessions.len() >= self.capacity
            && self.sessions.values().all(|runtime| Arc::strong_count(runtime) > 1) {
            return None;
        }
        let used = self.ingested_bytes.get(session).copied().unwrap_or(0);
        let total = used.checked_add(bytes)?;
        if total > MAX_INGEST_BODY { return None; }
        let runtime = self.route(session);
        self.ingested_bytes.insert(session.to_string(), total);
        Some(runtime)
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// A poisoned lock is a panicked turn, not corrupt state — the runtime is
/// single-writer, so the last completed write is intact. Recover rather than
/// kill the connection.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Serve scoped sessions over loopback. Authenticate each connection using
/// `AUTH <credential>`, then address sessions as `<workspace>/<session>`.
/// The operator policy is re-read before each operation.
pub fn serve(listener: TcpListener, router: Arc<Mutex<SessionRouter>>, policy: SessionPolicy) -> std::io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "session listener must bind to loopback"));
    }
    listener.set_nonblocking(true)?;
    let connections = Arc::new(());
    while policy.running() {
        match listener.accept() {
            Ok((stream, _)) => {
                if Arc::strong_count(&connections) > 32 || stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let router = Arc::clone(&router);
                let policy = policy.clone();
                let slot = Arc::clone(&connections);
                std::thread::spawn(move || {
                    let _slot = slot;
                    handle(stream, router, &policy);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(25)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

const MAX_INGEST_BODY: usize = 16 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FRAME: u64 = 16_384;

fn respond(stream: &mut TcpStream, text: &str) {
    drop(stream.write_all(text.as_bytes()));
}

fn read_frame(reader: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    let size = reader.take(MAX_FRAME + 1).read_line(&mut line).ok()? as u64;
    (size <= MAX_FRAME && line.ends_with('\n')).then(|| line.trim_end_matches(['\r', '\n']).to_string())
}

fn handle(mut stream: TcpStream, router: Arc<Mutex<SessionRouter>>, policy: &SessionPolicy) {
    let Ok(read_half) = stream.try_clone() else { return; };
    if read_half.set_read_timeout(Some(READ_TIMEOUT)).is_err() || stream.set_write_timeout(Some(READ_TIMEOUT)).is_err() { return; }
    let mut reader = BufReader::new(read_half);
    let login = read_frame(&mut reader);
    let Some(credential) = login.as_deref().and_then(|line| line.strip_prefix("AUTH ")) else {
        respond(&mut stream, "ERR authentication required\n");
        return;
    };
    if policy.authenticate(credential).is_none() {
        respond(&mut stream, "ERR authentication required\n");
        return;
    }
    respond(&mut stream, "OK\n");
    let mut gate = PolicyEnforcementPoint::default();
    for _ in 0..128 {
        let Some(line) = read_frame(&mut reader) else { return; };
        let mut parts = line.splitn(3, ' ');
        let (Some(command), Some(target), Some(argument)) = (parts.next(), parts.next(), parts.next()) else { return; };
        let Some((workspace, session)) = target.split_once('/') else { return; };
        let operation = match command { "ASK" => "ask", "INGEST" => "ingest", _ => return };
        let identity = policy.authenticate(credential);
        if !valid_label(session) || !gate.enforce(identity.as_ref(), operation, workspace) {
            respond(&mut stream, "ERR session access denied\n");
            return;
        }

        let body = if operation == "ingest" {
            let Some(length) = argument.trim().parse::<usize>().ok().filter(|n| *n <= MAX_INGEST_BODY) else {
                respond(&mut stream, "ERR invalid ingest length\n");
                return;
            };
            let mut body = vec![0u8; length];
            if reader.read_exact(&mut body).is_err() { return; }
            Some(body)
        } else {
            None
        };
        let Some(runtime) = lock(&router).route_request(target, body.as_ref().map_or(0, Vec::len)) else {
            respond(&mut stream, "ERR session capacity reached\n");
            continue;
        };
        let mut runtime = lock(&runtime);
        let identity = policy.authenticate(credential);
        if !gate.enforce(identity.as_ref(), operation, workspace) { return; }
        match body {
            Some(body) => match runtime.ingest(&String::from_utf8_lossy(&body), None) {
                Ok(_) => respond(&mut stream, "OK\n"),
                Err(e) => respond(&mut stream, &format!("ERR {e}\n")),
            },
            None => {
                let answer = runtime.ask(argument, None);
                respond(&mut stream, &format!("OK {}\n{}", answer.text.len(), answer.text));
            }
        }
    }
}
