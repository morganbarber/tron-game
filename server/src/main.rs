mod conn;
mod game;
mod lobby;
mod net;

use common::{lobby as lb, msg, TICK_HZ, R, W};
use conn::Conn;
use game::{Client, Game};
use lobby::{Lobbies, Lobby, Settings};
use net::Frame;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

struct Shared {
    lobbies: Lobbies,
    start: Instant,
    /// Ticks skipped after stalls, so `server_time` stays aligned with `Game::tick`.
    skipped: AtomicU64,
    web: PathBuf,
    /// Print tick timing every 10 s (`--stats`).
    stats: bool,
}

impl Shared {
    fn server_time(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * TICK_HZ - self.skipped.load(Ordering::Relaxed) as f64
    }
}

fn main() {
    let mut addr = "0.0.0.0:8080".to_string();
    let mut fill = 4usize;
    let mut rounds = common::ROUNDS;
    let mut stats = false;
    let cores = thread::available_parallelism().map_or(1, |n| n.get());
    let mut threads = cores.min(8);
    let mut per_ip = 3;
    let mut snapshot_hz = 60u64;
    let mut max_lobbies = 256;
    let mut web = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../web"));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--addr" => addr = args.next().expect("--addr ADDR"),
            "--bots" => fill = args.next().and_then(|v| v.parse().ok()).expect("--bots N"),
            "--rounds" => rounds = args.next().and_then(|v| v.parse().ok()).expect("--rounds N"),
            "--web" => web = args.next().expect("--web DIR").into(),
            "--stats" => stats = true,
            "--threads" => threads = args.next().and_then(|v| v.parse().ok()).expect("--threads N"),
            "--snapshot-hz" => snapshot_hz = args.next().and_then(|v| v.parse().ok()).filter(|&v: &u64| (1..=60).contains(&v)).expect("--snapshot-hz 1-60"),
            "--max-lobbies" => max_lobbies = args.next().and_then(|v| v.parse().ok()).expect("--max-lobbies N"),
            "--max-per-ip" => per_ip = args.next().and_then(|v| v.parse().ok()).expect("--max-per-ip N"),
            _ => {
                eprintln!("usage: server [--addr 0.0.0.0:8080] [--bots N (main lobby: fill rounds to N cycles)] [--rounds N (main lobby, default 10)] [--web DIR] [--threads N (simulation, default cores up to 8)] [--max-lobbies N (default 256)] [--max-per-ip N (lobbies one address may open, default 3)] [--snapshot-hz N (position updates, default 60)] [--stats]");
                std::process::exit(2);
            }
        }
    }

    let start = Instant::now();
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
    let snap_every = (TICK_HZ as u64 / snapshot_hz).max(1);
    let main = Game::new(fill, rounds, seed).snapshot_every(snap_every);
    let lobbies = Lobbies::new(main, seed.rotate_left(17), threads.max(1) - 1, per_ip).snapshot_every(snap_every).max_lobbies(max_lobbies);
    let shared = Arc::new(Shared { lobbies, start, skipped: AtomicU64::new(0), web, stats });

    {
        let shared = shared.clone();
        thread::Builder::new().name("tick".into()).spawn(move || tick_loop(&shared)).unwrap();
    }

    let listener = TcpListener::bind(&addr).expect("bind");
    println!("retro cycles listening on http://{addr}");
    let next_conn = AtomicU64::new(1);
    for stream in listener.incoming().flatten() {
        let shared = shared.clone();
        let conn = next_conn.fetch_add(1, Ordering::Relaxed);
        let _ = thread::Builder::new().stack_size(128 * 1024).spawn(move || {
            let _ = handle(stream, conn, &shared);
        });
    }
}

fn tick_loop(shared: &Shared) {
    let period = Duration::from_secs_f64(1.0 / TICK_HZ);
    let mut next = shared.start + period;
    let mut times = Vec::with_capacity(600);
    loop {
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        }
        let t = Instant::now();
        shared.lobbies.step();
        if shared.stats {
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            if times.len() == 600 {
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let mean = times.iter().sum::<f64>() / 600.0;
                let (lobbies, conns) = shared.lobbies.counts();
                eprintln!(
                    "tick: mean {mean:.3} ms  p99 {:.3}  max {:.3}  | {lobbies} lobbies, {conns} connections, {} ticks skipped",
                    times[594], times[599], shared.skipped.load(Ordering::Relaxed)
                );
                times.clear();
            }
        }
        next += period;
        // If we fell far behind (e.g. suspended), skip ahead instead of fast-forwarding.
        let now = Instant::now();
        if now > next + period * 30 {
            let n = ((now - next).as_secs_f64() * TICK_HZ) as u32;
            next += period * n;
            shared.skipped.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
}

fn handle(stream: TcpStream, conn: u64, shared: &Shared) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut reader = BufReader::with_capacity(4096, stream.try_clone()?);
    let req = net::read_request(&mut reader)?;
    let mut stream = stream;
    let Some(key) = req.ws_key else {
        return net::serve_file(&mut stream, &shared.web, &req.path);
    };
    net::accept_websocket(&mut stream, &key)?;

    let link = Arc::new(Conn::new(stream.try_clone()?));

    // A connection starts outside any lobby: it can list, create or enter one.
    let ip = stream.peer_addr().ok().map(|a| a.ip());
    let reply = |payload: Vec<u8>| {
        link.send(&net::frame(2, &payload));
    };
    let mut current: Option<Arc<Lobby>> = None;
    let mut last_attempt: Option<Instant> = None;
    let mut last_list: Option<Instant> = None;
    let enter = |current: &mut Option<Arc<Lobby>>, l: Arc<Lobby>| -> std::io::Result<()> {
        if let Some(old) = current.take() {
            old.game.lock().unwrap().remove_client(conn);
        }
        let mut w = W::new(msg::ENTERED);
        l.write(&mut w);
        reply(w.done());
        l.game.lock().unwrap().add_client(Client { conn, link: link.clone(), player: None });
        *current = Some(l);
        Ok(())
    };

    let result = (|| -> std::io::Result<()> {
        loop {
            match net::read_frame(&mut reader)? {
                Frame::Binary(data) => {
                    let mut r = R::new(&data);
                    match r.u8() {
                        msg::C_PING => {
                            // Answered without touching any game lock for the most honest RTT.
                            let client_time = r.f64();
                            let server_time = shared.server_time();
                            reply(W::new(msg::PONG).f64(client_time).f64(server_time).done());
                        }
                        msg::C_LIST => {
                            if last_list.map_or(true, |t| t.elapsed() >= Duration::from_millis(250)) {
                                last_list = Some(Instant::now());
                                reply(shared.lobbies.list_msg());
                            }
                        }
                        kind @ (msg::C_CREATE | msg::C_ENTER) => {
                            // One create / password attempt per second per connection.
                            if last_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
                                reply(W::new(msg::LOBBY_ERR).u8(lb::SLOW_DOWN).done());
                                continue;
                            }
                            let text = |r: &mut R, max: usize| {
                                let n = r.u8() as usize;
                                let b = r.bytes(n);
                                (n <= max * 4).then(|| String::from_utf8_lossy(b).into_owned())
                            };
                            let found = if kind == msg::C_CREATE {
                                last_attempt = Some(Instant::now());
                                let (bots, rounds, private) = (r.u8(), r.u8(), r.u8() != 0);
                                let (name, password) = (text(&mut r, lb::MAX_NAME), text(&mut r, lb::MAX_PASSWORD));
                                match (r.ok, name, password) {
                                    (true, Some(name), Some(password)) => shared.lobbies.create(ip, Settings { name, bots, rounds, private, password }),
                                    _ => Err(lb::INVALID),
                                }
                            } else {
                                let (code, password) = (text(&mut r, 8).unwrap_or_default(), text(&mut r, lb::MAX_PASSWORD).unwrap_or_default());
                                match shared.lobbies.find(&code) {
                                    None => Err(lb::NOT_FOUND),
                                    Some(l) if l.game.lock().unwrap().client_count() >= lobby::MAX_CONNECTIONS => Err(lb::FULL),
                                    Some(l) if l.password_ok(&password) => Ok(l),
                                    Some(_) => {
                                        if !password.is_empty() {
                                            last_attempt = Some(Instant::now());
                                        }
                                        Err(lb::BAD_PASSWORD)
                                    }
                                }
                            };
                            match found {
                                Ok(l) => enter(&mut current, l)?,
                                Err(e) => reply(W::new(msg::LOBBY_ERR).u8(e).done()),
                            }
                        }
                        msg::C_LEAVE => {
                            if let Some(old) = current.take() {
                                old.game.lock().unwrap().remove_client(conn);
                            }
                        }
                        _ => {
                            if let Some(l) = &current {
                                l.game.lock().unwrap().handle(conn, &data);
                            }
                        }
                    }
                }
                Frame::Ping(p) => {
                    link.send(&net::frame(10, &p));
                }
                Frame::Close => return Ok(()),
                Frame::Other => {}
            }
        }
    })();
    if let Some(l) = current {
        l.game.lock().unwrap().remove_client(conn);
    }
    link.close();
    let _ = stream.shutdown(std::net::Shutdown::Both);
    result
}
