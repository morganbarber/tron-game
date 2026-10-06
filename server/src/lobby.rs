//! Lobbies: independent games stepped by the one tick thread. Public lobbies
//! are listed; private ones are reachable only by their code. Either kind may
//! need a password.

use crate::game::Game;
use common::{lobby, msg, TICK_HZ, W};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

/// Code of the permanent public lobby that `--bots` / `--rounds` configure.
pub const MAIN_CODE: &str = "GRID";
/// Connections per lobby (players + spectators): bounds the work of one broadcast.
pub const MAX_CONNECTIONS: usize = 64;
/// Empty lobbies (other than the main one) close after this long.
const IDLE_TICKS: u64 = 60 * TICK_HZ as u64;
/// Unambiguous characters for lobby codes (no 0/O, 1/I).
const CODE_CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub struct Lobby {
    pub code: String,
    pub name: String,
    pub private: bool,
    password: String,
    owner: Option<IpAddr>,
    pub game: Mutex<Game>,
    /// Ticks since the last connection left (0 while anyone is in).
    idle: AtomicU64,
}

impl Lobby {
    pub fn password_ok(&self, attempt: &str) -> bool {
        if !self.locked() {
            return true;
        }
        // Constant-time over the stored length, so timing doesn't leak a prefix.
        let (a, b) = (self.password.as_bytes(), attempt.as_bytes());
        let diff = (0..a.len()).fold(a.len() ^ b.len(), |d, i| d | (a[i] ^ b.get(i).copied().unwrap_or(0)) as usize);
        diff == 0
    }

    pub fn locked(&self) -> bool {
        !self.password.is_empty()
    }

    /// The LOBBY record shared by LOBBIES and ENTERED.
    pub fn write(&self, w: &mut W) {
        let (humans, bots, watchers, round, rounds) = self.game.lock().unwrap().summary();
        let flags = (self.locked() as u8) * lobby::LOCKED
            | (self.private as u8) * lobby::PRIVATE
            | ((self.code == MAIN_CODE) as u8) * lobby::MAIN;
        w.u8(self.code.len() as u8).bytes(self.code.as_bytes());
        w.u8(humans).u8(bots).u8(watchers).u8(round).u8(rounds).u8(flags);
        w.u8(self.name.len() as u8).bytes(self.name.as_bytes());
    }
}

pub struct Settings {
    pub name: String,
    pub bots: u8,
    pub rounds: u8,
    pub private: bool,
    pub password: String,
}

pub struct Lobbies {
    list: Mutex<Vec<Arc<Lobby>>>,
    rng: AtomicU64,
    /// Lobbies one address may have open at once.
    per_ip: usize,
    max_lobbies: usize,
    /// Snapshot interval (ticks) for games created here.
    snap_every: u64,
    pool: Arc<Pool>,
}

/// Worker threads that step lobbies in parallel. Each tick the tick thread
/// publishes the lobby list, everyone (it included) claims lobbies one at a
/// time until none are left, and a barrier closes the tick.
struct Pool {
    work: Mutex<Vec<Arc<Lobby>>>,
    next: AtomicUsize,
    start: Barrier,
    done: Barrier,
}

impl Pool {
    fn new(workers: usize) -> Arc<Pool> {
        let pool = Arc::new(Pool {
            work: Mutex::new(Vec::new()),
            next: AtomicUsize::new(0),
            start: Barrier::new(workers + 1),
            done: Barrier::new(workers + 1),
        });
        for i in 0..workers {
            let pool = pool.clone();
            std::thread::Builder::new().name(format!("sim{i}")).stack_size(256 * 1024).spawn(move || loop {
                pool.start.wait();
                pool.run();
                pool.done.wait();
            }).expect("spawn sim worker");
        }
        pool
    }

    fn run(&self) {
        let work = self.work.lock().unwrap().clone();
        loop {
            let i = self.next.fetch_add(1, Ordering::Relaxed);
            let Some(l) = work.get(i) else { break };
            let mut g = l.game.lock().unwrap();
            g.step();
            if g.client_count() == 0 {
                l.idle.fetch_add(1, Ordering::Relaxed);
            } else {
                l.idle.store(0, Ordering::Relaxed);
            }
        }
    }
}

impl Lobbies {
    /// `workers` extra threads step lobbies alongside the tick thread.
    pub fn new(main: Game, seed: u64, workers: usize, per_ip: usize) -> Lobbies {
        let main = Lobby {
            code: MAIN_CODE.into(),
            name: "Public Grid".into(),
            private: false,
            password: String::new(),
            owner: None,
            game: Mutex::new(main),
            idle: AtomicU64::new(0),
        };
        Lobbies { list: Mutex::new(vec![Arc::new(main)]), rng: AtomicU64::new(seed | 1), per_ip, max_lobbies: 256, snap_every: 1, pool: Pool::new(workers) }
    }

    pub fn max_lobbies(mut self, n: usize) -> Lobbies {
        self.max_lobbies = n.max(1);
        self
    }

    pub fn snapshot_every(mut self, n: u64) -> Lobbies {
        self.snap_every = n.max(1);
        self
    }

    fn rand(&self) -> u64 {
        let mut x = self.rng.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng.store(x, Ordering::Relaxed);
        x
    }

    /// Steps every game, then closes lobbies that have sat empty too long.
    /// Holding the list lock throughout keeps every game on the same tick.
    pub fn step(&self) {
        let mut list = self.list.lock().unwrap();
        let pool = &self.pool;
        *pool.work.lock().unwrap() = list.clone();
        pool.next.store(0, Ordering::Relaxed);
        pool.start.wait();
        pool.run();
        pool.done.wait();
        list.retain(|l| l.code == MAIN_CODE || l.idle.load(Ordering::Relaxed) < IDLE_TICKS);
    }

    /// (lobbies, connections in them) for `--stats`.
    pub fn counts(&self) -> (usize, usize) {
        let list = self.list.lock().unwrap();
        (list.len(), list.iter().map(|l| l.game.lock().unwrap().client_count()).sum())
    }

    pub fn find(&self, code: &str) -> Option<Arc<Lobby>> {
        let code = code.trim().to_ascii_uppercase();
        self.list.lock().unwrap().iter().find(|l| l.code == code).cloned()
    }

    pub fn create(&self, owner: Option<IpAddr>, s: Settings) -> Result<Arc<Lobby>, u8> {
        let name: String = s.name.chars().filter(|c| !c.is_control()).take(lobby::MAX_NAME).collect();
        let name = name.trim();
        if name.is_empty() || s.password.len() > lobby::MAX_PASSWORD || s.rounds == 0 || s.rounds > lobby::MAX_ROUNDS {
            return Err(lobby::INVALID);
        }
        let mut list = self.list.lock().unwrap();
        let mine = list.iter().filter(|l| owner.is_some() && l.owner == owner).count();
        if list.len() >= self.max_lobbies || mine >= self.per_ip {
            return Err(lobby::TOO_MANY);
        }
        let code = loop {
            let mut r = self.rand();
            let code: String = (0..6).map(|_| {
                let c = CODE_CHARS[(r % CODE_CHARS.len() as u64) as usize] as char;
                r /= CODE_CHARS.len() as u64;
                c
            }).collect();
            if !list.iter().any(|l| l.code == code) {
                break code;
            }
        };
        // Start on the main lobby's tick so every game shares one clock.
        let tick = list[0].game.lock().unwrap().tick;
        let bots = (s.bots as usize).min(common::MAX_PLAYERS - 1);
        let mut game = Game::new(bots, s.rounds, self.rand()).exact_bots().snapshot_every(self.snap_every);
        game.tick = tick;
        let l = Arc::new(Lobby {
            code,
            name: name.into(),
            private: s.private,
            password: s.password,
            owner,
            game: Mutex::new(game),
            idle: AtomicU64::new(0),
        });
        list.push(l.clone());
        Ok(l)
    }

    /// Public lobbies, the main one first, then the busiest.
    pub fn list_msg(&self) -> Vec<u8> {
        let list: Vec<Arc<Lobby>> = self.list.lock().unwrap().iter().filter(|l| !l.private).cloned().collect();
        let mut rows: Vec<(bool, u8, Vec<u8>)> = list.iter().map(|l| {
            let mut w = W::new(0);
            l.write(&mut w);
            let row = w.done()[1..].to_vec();
            (l.code != MAIN_CODE, u8::MAX - row[l.code.len() + 1], row)
        }).collect();
        rows.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let mut w = W::new(msg::LOBBIES);
        w.u8(rows.len() as u8);
        for (_, _, row) in rows {
            w.bytes(&row);
        }
        w.done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(private: bool, password: &str) -> Settings {
        Settings { name: "Test".into(), bots: 3, rounds: 5, private, password: password.into() }
    }

    #[test]
    fn private_lobbies_are_unlisted_and_passwords_checked() {
        let lobbies = Lobbies::new(Game::new(4, 10, 1), 7, 0, 3);
        let ip: Option<IpAddr> = Some("10.0.0.1".parse().unwrap());
        let open = lobbies.create(ip, settings(false, "")).unwrap();
        let hidden = lobbies.create(ip, settings(true, "hunter2")).unwrap();
        assert_eq!(lobbies.create(ip, settings(false, "")).map(|_| ()), Ok(()));
        // Fourth from the same address is refused.
        assert_eq!(lobbies.create(ip, settings(false, "")).map(|_| ()), Err(lobby::TOO_MANY));

        let listed = lobbies.list_msg();
        let has = |code: &str| listed.windows(code.len()).any(|w| w == code.as_bytes());
        assert!(has(MAIN_CODE) && has(&open.code));
        assert!(!has(&hidden.code));

        let found = lobbies.find(&hidden.code.to_lowercase()).unwrap();
        assert!(found.locked());
        assert!(found.password_ok("hunter2"));
        assert!(!found.password_ok("hunter"));
        assert!(!found.password_ok("hunter22"));
        assert!(open.password_ok("") && open.password_ok("anything"));
        assert!(!found.password_ok(""));
    }

    #[test]
    fn empty_lobbies_close_but_main_stays() {
        let lobbies = Lobbies::new(Game::new(4, 10, 1), 7, 0, 3);
        let l = lobbies.create(None, settings(false, "")).unwrap();
        assert_eq!(l.game.lock().unwrap().tick, lobbies.find(MAIN_CODE).unwrap().game.lock().unwrap().tick);
        for _ in 0..IDLE_TICKS {
            lobbies.step();
        }
        assert!(lobbies.find(&l.code).is_none());
        assert!(lobbies.find(MAIN_CODE).is_some());
    }

    /// Capacity benchmark (not run by default):
    /// `cargo test --release -p server bench_lobbies -- --ignored --nocapture`
    /// Full 16-bot lobbies, each with one spectator so they don't idle.
    #[test]
    #[ignore]
    fn bench_lobbies() {
        use crate::conn::Conn;
        use crate::game::Client;
        use std::io::Read;
        use std::time::Instant;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8) - 1;
        for n in [1usize, 10, 50, 100, 200] {
            let lobbies = Lobbies::new(Game::new(0, 10, 1), 7, workers, 3);
            let mut peers = Vec::new();
            for i in 0..n {
                let l = lobbies.create(None, Settings { name: format!("b{i}"), bots: 16, rounds: 10, private: false, password: String::new() }).unwrap();
                let stream = std::net::TcpStream::connect(addr).unwrap();
                let peer = listener.accept().unwrap().0;
                peer.set_nonblocking(true).unwrap();
                peers.push(peer);
                l.game.lock().unwrap().add_client(Client { conn: i as u64 + 1, link: Arc::new(Conn::new(stream)), player: None });
            }
            let ticks = 60 * 120;
            let mut times = Vec::with_capacity(ticks);
            let mut bytes = 0usize;
            let mut buf = vec![0u8; 1 << 16];
            for _ in 0..ticks {
                let t = Instant::now();
                lobbies.step();
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                for p in peers.iter_mut() {
                    while let Ok(k) = p.read(&mut buf) {
                        if k == 0 {
                            break;
                        }
                        bytes += k;
                    }
                }
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mean = times.iter().sum::<f64>() / ticks as f64;
            let p99 = times[ticks * 99 / 100];
            println!(
                "{} threads, {n:>4} lobbies x16 bots: tick mean {mean:.3} ms  p99 {p99:.3} ms  max {:.3} ms  ({:.1} us/lobby)  {:.1} KB/s per spectator (incl. framing)",
                workers + 1,
                times[ticks - 1],
                mean * 1000.0 / n as f64,
                bytes as f64 / n as f64 / (ticks as f64 / 60.0) / 1024.0
            );
        }
    }

    #[test]
    fn bad_settings_are_rejected() {
        let lobbies = Lobbies::new(Game::new(4, 10, 1), 7, 0, 3);
        let mut s = settings(false, "");
        s.name = "   ".into();
        assert_eq!(lobbies.create(None, s).map(|_| ()), Err(lobby::INVALID));
        let mut s = settings(false, "");
        s.rounds = 0;
        assert_eq!(lobbies.create(None, s).map(|_| ()), Err(lobby::INVALID));
    }
}
