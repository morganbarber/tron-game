//! Authoritative game state. Turns are applied the moment they arrive (rewound to
//! the client's claimed time, within MAX_REWIND_TICKS) and broadcast immediately;
//! positions are broadcast every tick.

use crate::conn::Conn;
use crate::net::frame;
use common::*;
use std::collections::VecDeque;
use std::sync::Arc;

pub struct Client {
    pub conn: u64,
    pub link: Arc<Conn>,
    pub player: Option<u8>,
}

struct Player {
    name: String,
    color: [u8; 3],
    score: i16,
    kills: u16,
    deaths: u16,
    bot: bool,
}

struct Cycle {
    id: u8,
    /// Stuck at any point since the last snapshot, so a brief scrape isn't
    /// missed when snapshots are sent less often than every tick.
    stuck_seen: bool,
    alive: bool,
    walls: bool,
    x: f32,
    y: f32,
    dir: u8,
    speed: f32,
    /// Spawn point followed by every turn point; the live segment runs from the last one to (x, y).
    trail: Vec<(f32, f32)>,
    /// Time (ticks) the live segment started.
    seg_t0: f64,
    /// (time, distance along live segment) samples for rewinding turns.
    hist: VecDeque<(f64, f32)>,
    stuck: u32,
    death_tick: u64,
    bot_cooldown: u32,
    bot_look: f32,
    /// Bot skill: ticks between looks at the world, and chance to miss danger on a look.
    bot_react: u32,
    bot_miss: f32,
    bot_think_at: u64,
    bot_last_turn: u64,
    /// Turns stamped between the last simulated tick and the next: (time, left, seq).
    queued: Vec<(f64, bool, u8)>,
    braking: bool,
    /// Brake energy, 0..1.
    brake: f32,
}

impl Cycle {
    fn dist_at(&self, t: f64) -> f32 {
        let mut prev = self.hist[0];
        if t <= prev.0 {
            return prev.1;
        }
        for &cur in self.hist.iter().skip(1) {
            if t <= cur.0 {
                let span = cur.0 - prev.0;
                let f = if span > 0.0 { ((t - prev.0) / span) as f32 } else { 1.0 };
                return prev.1 + (cur.1 - prev.1) * f;
            }
            prev = cur;
        }
        prev.1
    }

    fn effective_speed(&self) -> f32 {
        if self.stuck > 0 { 0.0 } else { self.speed }
    }

    fn flags(&self) -> u8 {
        (self.alive as u8) * FLAG_ALIVE | ((self.stuck > 0 || self.stuck_seen) as u8) * FLAG_STUCK | (self.braking as u8) * FLAG_BRAKING
    }
}

const PALETTE: [[u8; 3]; 8] = [
    [255, 140, 20],
    [30, 200, 255],
    [255, 50, 90],
    [120, 255, 60],
    [190, 90, 255],
    [255, 230, 50],
    [40, 255, 200],
    [255, 110, 210],
];
const BOT_NAMES: [&str; 8] = ["Clu", "Sark", "Rinzler", "Tesler", "Crom", "Ram", "Yori", "Dumont"];

pub struct Game {
    pub tick: u64,
    phase: u8,
    phase_end: u64,
    /// 1-based round within the current match.
    round: u8,
    rounds: u8,
    /// Sudden-death zone inset from the rim, and when it starts closing.
    zone: f32,
    zone_start: u64,
    players: Vec<Option<Player>>,
    cycles: Vec<Cycle>,
    clients: Vec<Client>,
    /// Bot target: total cycles to fill up to, or (when `exact_bots`) bots to add.
    fill: usize,
    exact_bots: bool,
    /// Ticks between snapshots while cycles move (1 = every tick).
    snap_every: u64,
    rng: u64,
    /// Every wall, kept up to date by `build_world`.
    world: World,
    /// Trail points of each player already filed in `world` as finished walls.
    indexed: [usize; MAX_PLAYERS],
    /// False when walls have gone away, so `world` must start over.
    world_ok: bool,
}

impl Game {
    pub fn new(fill: usize, rounds: u8, seed: u64) -> Game {
        Game {
            tick: 0,
            phase: phase::WAITING,
            phase_end: 0,
            round: 1,
            rounds: rounds.max(1),
            zone: 0.0,
            zone_start: u64::MAX,
            players: (0..MAX_PLAYERS).map(|_| None).collect(),
            cycles: Vec::new(),
            clients: Vec::new(),
            fill: fill.min(MAX_PLAYERS),
            exact_bots: false,
            snap_every: 1,
            rng: seed | 1,
            world: World::default(),
            indexed: [0; MAX_PLAYERS],
            world_ok: false,
        }
    }

    /// Always play with exactly `fill` bots (as far as free slots allow),
    /// instead of only topping up to `fill` cycles.
    pub fn exact_bots(mut self) -> Game {
        self.exact_bots = true;
        self
    }

    /// Send positions every `n` ticks instead of every tick. Turns, deaths and
    /// everything else still go out the moment they happen.
    pub fn snapshot_every(mut self, n: u64) -> Game {
        self.snap_every = n.max(1);
        self
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// (humans, bots, spectators, round, rounds) for the lobby list.
    pub fn summary(&self) -> (u8, u8, u8, u8, u8) {
        let humans = self.players.iter().flatten().filter(|p| !p.bot).count();
        let bots = if self.exact_bots { self.fill.min(MAX_PLAYERS - humans) } else { self.fill.saturating_sub(humans) };
        let watchers = self.clients.iter().filter(|c| c.player.is_none()).count();
        (humans as u8, bots as u8, watchers.min(255) as u8, self.round, self.rounds)
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    // ---------------------------------------------------------------- networking

    // A client too far behind to be playable is dropped by `Conn::send`;
    // its reader thread then removes it.
    fn broadcast(&mut self, payload: &[u8]) {
        let out = frame(2, payload);
        for c in &self.clients {
            c.link.send(&out);
        }
    }

    fn send_to(&self, conn: u64, payload: &[u8]) {
        if let Some(c) = self.clients.iter().find(|c| c.conn == conn) {
            c.link.send(&frame(2, payload));
        }
    }

    pub fn add_client(&mut self, client: Client) {
        let conn = client.conn;
        self.clients.push(client);
        self.send_to(conn, &W::new(msg::WELCOME).u8(NO_ID).done());
        self.send_to(conn, &self.roster_msg());
        self.send_to(conn, &self.full_msg());
    }

    pub fn remove_client(&mut self, conn: u64) {
        let Some(i) = self.clients.iter().position(|c| c.conn == conn) else { return };
        let c = self.clients.swap_remove(i);
        if let Some(id) = c.player {
            if let Some(ci) = self.cycles.iter().position(|c| c.id == id) {
                if self.cycles[ci].alive {
                    self.kill(ci, NO_ID);
                }
                self.cycles[ci].walls = false;
                self.world_ok = false;
                self.broadcast(&W::new(msg::WALLS_GONE).u8(id).done());
            }
            self.players[id as usize] = None;
            let roster = self.roster_msg();
            self.broadcast(&roster);
        }
    }

    pub fn handle(&mut self, conn: u64, data: &[u8]) {
        let mut r = R::new(data);
        match r.u8() {
            msg::C_JOIN => {
                let color = [r.u8(), r.u8(), r.u8()];
                let n = r.u8() as usize;
                let name: String = String::from_utf8_lossy(r.bytes(n)).chars().filter(|c| !c.is_control()).take(16).collect();
                self.join(conn, if name.trim().is_empty() { "Program".into() } else { name }, color);
            }
            msg::C_BRAKE => {
                let on = r.u8() != 0;
                let Some(id) = self.clients.iter().find(|c| c.conn == conn).and_then(|c| c.player) else { return };
                if let Some(c) = self.cycles.iter_mut().find(|c| c.id == id) {
                    c.braking = on;
                }
            }
            msg::C_TURN => {
                let seq = r.u8();
                let left = r.u8() != 0;
                let t = r.f64();
                if !r.ok || !t.is_finite() {
                    return;
                }
                let Some(id) = self.clients.iter().find(|c| c.conn == conn).and_then(|c| c.player) else { return };
                if let Some(ci) = self.cycles.iter().position(|c| c.id == id) {
                    let now = self.tick as f64;
                    let c = &mut self.cycles[ci];
                    if self.phase != phase::COUNTDOWN && (t > now || !c.queued.is_empty()) && c.queued.len() < 8 {
                        // The client is (correctly) ahead of our last simulated tick: apply
                        // exactly once that moment has been simulated.
                        c.queued.push((t.min(now + 1.0), left, seq));
                    } else {
                        self.turn(ci, left, t, seq);
                    }
                }
            }
            _ => {}
        }
    }

    fn join(&mut self, conn: u64, name: String, color: [u8; 3]) {
        let Some(ci) = self.clients.iter().position(|c| c.conn == conn) else { return };
        if let Some(id) = self.clients[ci].player {
            // Already playing: this is a name / colour change.
            if let Some(p) = self.players[id as usize].as_mut() {
                p.name = name;
                p.color = color;
            }
            let roster = self.roster_msg();
            self.broadcast(&roster);
            return;
        }
        // Take a free slot, or evict a bot if the server is full of them.
        let slot = self.players.iter().position(|p| p.is_none())
            .or_else(|| self.players.iter().position(|p| p.as_ref().is_some_and(|p| p.bot)));
        let Some(id) = slot else { return };
        self.players[id] = Some(Player { name, color, score: 0, kills: 0, deaths: 0, bot: false });
        self.clients[ci].player = Some(id as u8);
        self.send_to(conn, &W::new(msg::WELCOME).u8(id as u8).done());
        // Get the new player in quickly unless real humans are mid-round.
        let humans_racing = self.cycles.iter().any(|c| {
            c.alive && self.players[c.id as usize].as_ref().is_some_and(|p| !p.bot)
        });
        let restart = match self.phase {
            phase::WAITING | phase::COUNTDOWN => true,
            phase::PLAYING => !humans_racing,
            _ => false, // the next round starts shortly anyway
        };
        if restart {
            self.start_round();
        } else {
            let roster = self.roster_msg();
            self.broadcast(&roster);
        }
    }

    fn roster_msg(&self) -> Vec<u8> {
        let mut w = W::new(msg::ROSTER);
        w.u8(self.players.iter().filter(|p| p.is_some()).count() as u8);
        for (id, p) in self.players.iter().enumerate() {
            if let Some(p) = p {
                let name = &p.name.as_bytes()[..p.name.len().min(64)];
                w.u8(id as u8).bytes(&p.color).u8(p.bot as u8).i16(p.score).u16(p.kills).u16(p.deaths);
                w.u8(name.len() as u8).bytes(name);
            }
        }
        w.0
    }

    fn write_cycle(&self, w: &mut W, c: &Cycle) {
        // Cycles hold still during the countdown; tell clients so they don't extrapolate.
        let moving = self.phase == phase::PLAYING || self.phase == phase::ROUND_OVER;
        let speed = if moving { c.effective_speed() } else { 0.0 };
        w.u8(c.id).u8(c.flags() | c.dir << 4).u16(pos_q(c.x)).u16(pos_q(c.y));
        w.u16((speed * 256.0).round() as u16).u8((c.brake * 255.0) as u8);
    }

    fn full_msg(&self) -> Vec<u8> {
        let mut w = W::new(msg::FULL);
        w.u32(self.tick as u32).u8(self.phase).u32(self.phase_end as u32).u8(self.round).u8(self.rounds);
        let shown: Vec<&Cycle> = self.cycles.iter().filter(|c| c.walls || c.alive).collect();
        w.u8(shown.len() as u8);
        for c in shown {
            self.write_cycle(&mut w, c);
            w.u16(c.trail.len() as u16);
            for &(x, y) in &c.trail {
                w.f32(x).f32(y);
            }
        }
        w.0
    }

    fn phase_msg(&self, winner: u8) -> Vec<u8> {
        W::new(msg::PHASE).u8(self.phase).u32(self.phase_end as u32).u8(winner).u8(self.round).u8(self.rounds).done()
    }

    // ---------------------------------------------------------------- rounds

    fn start_round(&mut self) {
        let humans = self.players.iter().flatten().filter(|p| !p.bot).count();
        let bots_wanted = if self.exact_bots { self.fill.min(MAX_PLAYERS - humans) } else { self.fill.saturating_sub(humans) };
        // Bots take palette colours that don't clash with any human's pick.
        let human_cols: Vec<[u8; 3]> = self.players.iter().flatten().filter(|p| !p.bot).map(|p| p.color).collect();
        let clash = |c: [u8; 3]| {
            human_cols.iter().any(|h| (0..3).map(|k| (h[k] as i32 - c[k] as i32).abs()).sum::<i32>() < 140)
        };
        let mut free_cols: Vec<[u8; 3]> = PALETTE.iter().copied().filter(|&c| !clash(c)).collect();
        free_cols.extend(PALETTE.iter().copied().filter(|&c| clash(c)));
        let mut bots = 0;
        for i in 0..MAX_PLAYERS {
            match &self.players[i] {
                Some(p) if p.bot => {
                    if bots < bots_wanted {
                        let col = free_cols[bots % free_cols.len()];
                        self.players[i].as_mut().unwrap().color = col;
                        bots += 1;
                    } else {
                        self.players[i] = None
                    }
                }
                None if bots < bots_wanted => {
                    self.players[i] = Some(Player {
                        name: BOT_NAMES[i % BOT_NAMES.len()].into(),
                        color: free_cols[bots % free_cols.len()],
                        score: 0,
                        kills: 0,
                        deaths: 0,
                        bot: true,
                    });
                    bots += 1;
                }
                _ => {}
            }
        }

        let ids: Vec<u8> = (0..MAX_PLAYERS as u8).filter(|&i| self.players[i as usize].is_some()).collect();
        self.cycles.clear();
        self.world_ok = false;
        self.zone = 0.0;
        self.zone_start = u64::MAX;
        if ids.is_empty() {
            self.phase = phase::WAITING;
        } else {
            let n = ids.len();
            let rot = self.rand() * std::f32::consts::TAU;
            // Shuffle seats so nobody always starts in the same place.
            let mut seats: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                let j = (self.rand() * (i + 1) as f32) as usize % (i + 1);
                seats.swap(i, j);
            }
            for (k, &id) in ids.iter().enumerate() {
                let th = rot + seats[k] as f32 / n as f32 * std::f32::consts::TAU;
                let c = ARENA * 0.5;
                let (x, y) = ((c + th.cos() * ARENA * 0.33).round(), (c + th.sin() * ARENA * 0.33).round());
                let (tx, ty) = (-th.sin(), th.cos());
                let dir = (0..4u8).max_by(|&a, &b| {
                    let da = DIRS[a as usize].0 * tx + DIRS[a as usize].1 * ty;
                    let db = DIRS[b as usize].0 * tx + DIRS[b as usize].1 * ty;
                    da.total_cmp(&db)
                }).unwrap();
                let look = 0.2 + self.rand() * 0.2;
                let react = 6 + (self.rand() * 9.0) as u32; // 100-250 ms
                let miss = 0.08 + self.rand() * 0.17;
                self.cycles.push(Cycle {
                    id,
                    alive: true,
                    walls: true,
                    x,
                    y,
                    dir,
                    speed: BASE_SPEED,
                    trail: vec![(x, y)],
                    seg_t0: self.tick as f64,
                    hist: VecDeque::from([(self.tick as f64, 0.0)]),
                    stuck: 0,
                    stuck_seen: false,
                    death_tick: 0,
                    bot_cooldown: 0,
                    bot_look: look,
                    bot_react: react,
                    bot_miss: miss,
                    bot_think_at: 0,
                    bot_last_turn: 0,
                    queued: Vec::new(),
                    braking: false,
                    brake: 1.0,
                });
            }
            self.phase = phase::COUNTDOWN;
            self.phase_end = self.tick + 3 * TICK_HZ as u64;
        }
        let roster = self.roster_msg();
        self.broadcast(&roster);
        let full = self.full_msg();
        self.broadcast(&full);
    }

    fn end_round(&mut self) {
        let alive: Vec<u8> = self.cycles.iter().filter(|c| c.alive).map(|c| c.id).collect();
        let winner = if alive.len() == 1 && self.cycles.len() > 1 { alive[0] } else { NO_ID };
        if let Some(p) = self.players.get_mut(winner as usize).and_then(|p| p.as_mut()) {
            p.score += score::WIN;
        }
        if self.round >= self.rounds {
            self.phase = phase::MATCH_OVER;
            self.phase_end = self.tick + 8 * TICK_HZ as u64;
        } else {
            self.phase = phase::ROUND_OVER;
            self.phase_end = self.tick + 3 * TICK_HZ as u64;
        }
        let m = self.phase_msg(winner);
        self.broadcast(&m);
        let roster = self.roster_msg();
        self.broadcast(&roster);
    }

    pub fn step(&mut self) {
        self.tick += 1;
        if self.clients.is_empty() {
            // Nobody watching: idle, but forget the board so the next visitor starts fresh.
            if self.phase != phase::WAITING {
                self.phase = phase::WAITING;
                self.round = 1;
                self.cycles.clear();
                self.world_ok = false;
                for p in self.players.iter_mut() {
                    if p.as_ref().is_some_and(|p| p.bot) {
                        *p = None;
                    }
                }
            }
            return;
        }
        match self.phase {
            phase::WAITING => self.start_round(),
            phase::COUNTDOWN => {
                if self.tick >= self.phase_end {
                    self.phase = phase::PLAYING;
                    self.phase_end = self.tick + 180 * TICK_HZ as u64;
                    self.zone_start = self.tick + ZONE_START_TICKS;
                    let now = self.tick as f64;
                    for c in &mut self.cycles {
                        c.seg_t0 = now;
                        c.hist = VecDeque::from([(now, 0.0)]);
                    }
                    let m = self.phase_msg(NO_ID);
                    self.broadcast(&m);
                }
            }
            phase::PLAYING | phase::ROUND_OVER | phase::MATCH_OVER => {
                if self.phase == phase::PLAYING {
                    self.update_zone();
                }
                self.bots_think();
                self.simulate();
                for ci in 0..self.cycles.len() {
                    for (t, left, seq) in std::mem::take(&mut self.cycles[ci].queued) {
                        self.turn(ci, left, t, seq);
                    }
                }
                if self.phase == phase::PLAYING {
                    // The round runs until one cycle is left (or nobody, solo).
                    let alive = self.cycles.iter().filter(|c| c.alive).count();
                    if alive == 0 || (self.cycles.len() > 1 && alive <= 1) || self.tick >= self.phase_end {
                        self.end_round();
                    }
                } else if self.tick >= self.phase_end {
                    if self.phase == phase::MATCH_OVER {
                        self.round = 1;
                        for p in self.players.iter_mut().flatten() {
                            p.score = 0;
                            p.kills = 0;
                            p.deaths = 0;
                        }
                    } else {
                        self.round += 1;
                    }
                    self.start_round();
                }
            }
            _ => {}
        }

        // Walls of the fallen linger briefly, then derez.
        for i in 0..self.cycles.len() {
            let c = &self.cycles[i];
            if !c.alive && c.walls && self.tick >= c.death_tick + WALL_LINGER_TICKS {
                self.cycles[i].walls = false;
                self.world_ok = false;
                let id = self.cycles[i].id;
                self.broadcast(&W::new(msg::WALLS_GONE).u8(id).done());
            }
        }

        // Every `snap_every` ticks while cycles move; a few times a second otherwise.
        let moving = self.phase == phase::PLAYING || self.phase == phase::ROUND_OVER;
        let every = if moving { self.snap_every } else { 15 };
        if self.tick % every != 0 {
            return;
        }
        let mut w = W::new(msg::SNAPSHOT);
        w.u32(self.tick as u32).u16(pos_q(self.zone)).u8(self.cycles.iter().filter(|c| c.alive).count() as u8);
        for c in self.cycles.iter().filter(|c| c.alive) {
            self.write_cycle(&mut w, c);
        }
        for c in self.cycles.iter_mut() {
            c.stuck_seen = false;
        }
        self.broadcast(&w.0);
    }

    // ---------------------------------------------------------------- simulation

    /// Brings `world` up to date: files trail points laid since last time as
    /// finished walls, and refreshes the moving ones (growing segments, zone).
    fn build_world(&mut self) {
        let stale = !self.world_ok
            || self.cycles.iter().any(|c| {
                let n = self.indexed[c.id as usize];
                c.trail.len() < n || (n > 0 && !c.walls)
            });
        if stale {
            self.world.reset();
            self.indexed = [0; MAX_PLAYERS];
            self.world_ok = true;
        }
        self.world.moving.clear();
        if self.zone > 0.0 {
            self.world.moving.extend_from_slice(&square(self.zone, ZONE_ID));
        }
        for c in &self.cycles {
            if !c.walls {
                continue;
            }
            let done = &mut self.indexed[c.id as usize];
            for k in (*done).max(1)..c.trail.len() {
                let (a, b) = (c.trail[k - 1], c.trail[k]);
                self.world.add_fixed(Seg { ax: a.0, ay: a.1, bx: b.0, by: b.1, owner: c.id, current: false });
            }
            *done = c.trail.len();
            let (sx, sy) = *c.trail.last().unwrap();
            self.world.moving.push(Seg { ax: sx, ay: sy, bx: c.x, by: c.y, owner: c.id, current: true });
        }
    }

    fn kill(&mut self, ci: usize, killer: u8) {
        let c = &mut self.cycles[ci];
        c.alive = false;
        c.death_tick = self.tick;
        let (id, x, y, dir) = (c.id, c.x, c.y, c.dir);
        let suicide = killer == id || killer == NO_ID;
        let boxed = !suicide && killer != ZONE_ID && self.boxed(id, x, y, dir);
        if self.phase == phase::PLAYING {
            if let Some(p) = self.players[id as usize].as_mut() {
                p.deaths += 1;
                p.score += if suicide { score::SUICIDE } else { score::DEATH };
            }
            if !suicide {
                if let Some(p) = self.players.get_mut(killer as usize).and_then(|p| p.as_mut()) {
                    p.kills += 1;
                    p.score += score::KILL + if boxed { score::BOX } else { 0 };
                }
            }
            let roster = self.roster_msg();
            self.broadcast(&roster);
        }
        self.broadcast(&W::new(msg::DEATH).u8(id).u8(killer).f32(x).f32(y).u8(if boxed { DEATH_BOXED } else { 0 }).done());
    }

    /// Walls close on both sides of a crash: the victim had nowhere left to go.
    /// `world` holds this tick's walls whenever someone crashes into one.
    fn boxed(&mut self, id: u8, x: f32, y: f32, dir: u8) -> bool {
        let skip = |s: &Seg| s.owner == id && s.current;
        self.world.raycast(x, y, turn(dir, true), BOX_RANGE, skip) < BOX_RANGE
            && self.world.raycast(x, y, turn(dir, false), BOX_RANGE, skip) < BOX_RANGE
    }

    fn simulate(&mut self) {
        let dt = (1.0 / TICK_HZ) as f32;
        let now = self.tick as f64;

        // Speed: grinding along a wall accelerates, open ground decays toward base.
        self.build_world();
        for c in self.cycles.iter_mut().filter(|c| c.alive) {
            let dx = DIRS[c.dir as usize].0;
            let mut nearest = GRIND_RANGE;
            let (id, x, y) = (c.id, c.x, c.y);
            self.world.near(x, y, GRIND_RANGE, |s| {
                if s.owner == id && s.current {
                    return;
                }
                // Walls parallel to our heading that run alongside us.
                let d = if dx != 0.0 {
                    if (s.ay - s.by).abs() > 1e-6 || x < s.ax.min(s.bx) || x > s.ax.max(s.bx) { return }
                    (s.ay - y).abs()
                } else {
                    if (s.ax - s.bx).abs() > 1e-6 || y < s.ay.min(s.by) || y > s.ay.max(s.by) { return }
                    (s.ax - x).abs()
                };
                nearest = nearest.min(d);
            });
            let accel = GRIND_ACCEL * (1.0 - nearest / GRIND_RANGE);
            let braking = c.braking && c.brake > 0.0;
            if braking {
                c.brake = (c.brake - BRAKE_DRAIN * dt).max(0.0);
                c.speed = (c.speed - BRAKE_DECEL * dt).max(BRAKE_SPEED);
            } else {
                c.brake = (c.brake + BRAKE_RECHARGE * dt).min(1.0);
                if c.speed < BASE_SPEED {
                    // Recover from braking quickly but not instantly.
                    c.speed = (c.speed + 60.0 * dt).min(BASE_SPEED);
                }
            }
            if c.speed >= BASE_SPEED {
                c.speed = (c.speed + accel * dt - (c.speed - BASE_SPEED) * SPEED_DECAY * dt).clamp(BASE_SPEED, MAX_SPEED);
            }
        }

        // Move everyone, then resolve collisions against the updated walls.
        let mut paths = Vec::with_capacity(self.cycles.len());
        for c in self.cycles.iter_mut() {
            paths.push((c.x, c.y));
            if c.alive {
                let (dx, dy) = DIRS[c.dir as usize];
                let step = c.speed * dt;
                c.x += dx * step;
                c.y += dy * step;
                let d = c.hist.back().unwrap().1 + step;
                c.hist.push_back((now, d));
                while c.hist.len() > 2 && c.hist[1].0 < now - MAX_REWIND_TICKS - 1.0 {
                    c.hist.pop_front();
                }
            }
        }
        self.build_world();
        let mut deaths = Vec::new();
        for (i, c) in self.cycles.iter_mut().enumerate() {
            if !c.alive {
                continue;
            }
            let (x0, y0) = paths[i];
            let id = c.id;
            match self.world.first_hit(x0, y0, c.x, c.y, |s| s.owner == id && s.current) {
                Some((t, owner)) => {
                    let (dx, dy) = DIRS[c.dir as usize];
                    let full = c.speed * dt;
                    let ok = (full * t - 0.02).max(0.0);
                    c.x = x0 + dx * ok;
                    c.y = y0 + dy * ok;
                    c.hist.back_mut().unwrap().1 -= full - ok;
                    c.stuck += 1;
                    c.stuck_seen = true;
                    if c.stuck >= STUCK_GRACE_TICKS {
                        deaths.push((i, owner));
                    }
                }
                None => c.stuck = 0,
            }
        }
        // The closing zone takes anyone it has passed.
        let (lo, hi) = (self.zone - 0.05, ARENA - self.zone + 0.05);
        for (i, c) in self.cycles.iter().enumerate() {
            if c.alive && self.zone > 0.0 && (c.x < lo || c.x > hi || c.y < lo || c.y > hi) && !deaths.iter().any(|d| d.0 == i) {
                deaths.push((i, ZONE_ID));
            }
        }
        for (i, killer) in deaths {
            self.kill(i, killer);
        }
    }

    /// Sudden death: once the round drags on (or only bots are left), the zone closes in.
    fn update_zone(&mut self) {
        let human = |c: &Cycle| self.players[c.id as usize].as_ref().is_some_and(|p| !p.bot);
        let humans = self.cycles.iter().filter(|c| human(c)).count();
        let humans_alive = self.cycles.iter().filter(|c| c.alive && human(c)).count();
        let humans_out = humans > 0 && humans_alive == 0;
        if humans_out {
            self.zone_start = self.zone_start.min(self.tick + ZONE_AFTER_HUMANS_TICKS);
        }
        if self.tick >= self.zone_start {
            let speed = if humans_out { ZONE_FAST_SPEED } else { ZONE_SPEED };
            self.zone = (self.zone + speed / TICK_HZ as f32).min(ARENA * 0.5 - 4.0);
        }
    }

    /// Turns cycle `ci` at claimed time `t` (fractional ticks), rewinding if needed.
    fn turn(&mut self, ci: usize, left: bool, t: f64, seq: u8) {
        if !self.cycles[ci].alive {
            return;
        }
        let now = self.tick as f64;
        let moving = self.phase == phase::PLAYING || self.phase == phase::ROUND_OVER;
        if moving {
            self.build_world();
        }
        let c = &mut self.cycles[ci];
        let new_dir = turn(c.dir, left);
        if !moving {
            // Countdown: just face a new way on the spot.
            c.dir = new_dir;
            let (id, x, y) = (c.id, c.x, c.y);
            self.broadcast(&W::new(msg::TURN).u8(id).u8(seq).u8(new_dir).f32(x).f32(y).f64(now).done());
            return;
        }
        let tau = t.min(now).max(c.seg_t0).max(now - MAX_REWIND_TICKS);
        let d_tau = c.dist_at(tau);
        let d_now = c.hist.back().unwrap().1;
        let (sx, sy) = *c.trail.last().unwrap();
        let (dx, dy) = DIRS[c.dir as usize];
        let (tx, ty) = (sx + dx * d_tau, sy + dy * d_tau);
        c.trail.push((tx, ty));
        c.dir = new_dir;
        c.seg_t0 = tau;
        // Distance already covered since tau now continues in the new direction.
        let mut rem = d_now - d_tau;
        let (nx, ny) = DIRS[new_dir as usize];
        let id = c.id;
        if rem > 0.0 {
            if let Some((h, _)) = self.world.first_hit(tx, ty, tx + nx * rem, ty + ny * rem, |s| s.owner == id && s.current) {
                rem = (rem * h - 0.02).max(0.0);
                c.stuck = c.stuck.max(1);
                c.stuck_seen = true;
            } else {
                // Rewound to before the impact and got away clean.
                c.stuck = 0;
            }
        }
        // A turn on the spot doesn't reset the crash timer: only actually driving
        // free does (in `simulate`), so you can't wriggle forever when boxed in.
        c.x = tx + nx * rem;
        c.y = ty + ny * rem;
        c.hist.clear();
        c.hist.push_back((tau, 0.0));
        c.hist.push_back((now, rem));
        self.broadcast(&W::new(msg::TURN).u8(id).u8(seq).u8(new_dir).f32(tx).f32(ty).f64(tau).done());
    }

    fn bots_think(&mut self) {
        self.build_world();
        let now = self.tick as f64;
        let mut turns = Vec::new();
        for ci in 0..self.cycles.len() {
            let c = &self.cycles[ci];
            if !c.alive || !self.players[c.id as usize].as_ref().is_some_and(|p| p.bot) {
                continue;
            }
            // Bots only look at the world every so often, like a human's reaction time.
            if self.tick < c.bot_think_at {
                continue;
            }
            let id = c.id;
            let skip = |s: &Seg| s.owner == id && s.current;
            let look = c.speed * c.bot_look + 1.5;
            let ahead = self.world.raycast(c.x, c.y, c.dir, 300.0, skip);
            let (x, y, dir, cooldown, react, miss, last) = (c.x, c.y, c.dir, c.bot_cooldown, c.bot_react, c.bot_miss, c.bot_last_turn);
            let jitter = (self.rand() * 4.0) as u64;
            self.cycles[ci].bot_think_at = self.tick + react as u64 + jitter;
            self.cycles[ci].bot_cooldown = cooldown.saturating_sub(react);
            // Can't flick turns faster than a human could, and sometimes misread the danger.
            if self.tick < last + 8 {
                continue;
            }
            let emergency = ahead < look && self.rand() >= miss;
            let roll = self.rand();
            if emergency || (cooldown == 0 && roll < 0.06) {
                let l = self.world.raycast(x, y, turn(dir, true), 300.0, skip);
                let r = self.world.raycast(x, y, turn(dir, false), 300.0, skip);
                let misjudge = self.rand() < miss;
                let pick_left = if emergency {
                    // Picks the roomier side, except when it misjudges.
                    (l > ahead || r > ahead).then_some((l >= r) != misjudge)
                } else if l > 25.0 && r > 25.0 && ahead > 25.0 {
                    Some(self.rand() < 0.5)
                } else {
                    None
                };
                if let Some(left) = pick_left {
                    turns.push((ci, left));
                }
            }
        }
        for (ci, left) in turns {
            self.cycles[ci].bot_cooldown = 40;
            self.cycles[ci].bot_last_turn = self.tick;
            self.turn(ci, left, now, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cycle(id: u8, x: f32, y: f32, dir: u8, trail: Vec<(f32, f32)>, alive: bool) -> Cycle {
        let (sx, sy) = *trail.last().unwrap();
        let d = (x - sx).abs() + (y - sy).abs();
        Cycle {
            id, alive, walls: true, x, y, dir, speed: BASE_SPEED, trail, seg_t0: 0.0,
            hist: VecDeque::from([(0.0, d)]), stuck: 0, stuck_seen: false, death_tick: 0, bot_cooldown: 0, bot_look: 0.4,
            bot_react: 8, bot_miss: 0.0, bot_think_at: 0, bot_last_turn: 0, queued: Vec::new(), braking: false, brake: 1.0,
        }
    }

    /// Boxed in (rim ahead and below, a wall above, own trail behind): mashing
    /// turns every tick must not keep the cycle alive.
    #[test]
    fn boxed_in_cycle_dies_even_while_turning() {
        let mut g = Game::new(0, 10, 7);
        g.phase = phase::PLAYING;
        let a = ARENA;
        g.cycles.push(cycle(0, a - 0.5, 1.0, 0, vec![(a - 20.0, 1.0)], true));
        g.cycles.push(cycle(1, a, 1.6, 2, vec![(a, 1.6), (a - 30.0, 1.6)], false));
        g.cycles[1].x = a - 30.0;
        for i in 0..(STUCK_GRACE_TICKS * 3) {
            g.tick += 1;
            g.simulate();
            if !g.cycles[0].alive {
                return;
            }
            g.turn(0, i % 2 == 0, g.tick as f64, 0);
        }
        panic!("boxed-in cycle survived by turning");
    }

    /// Worst-case sim cost: 16 live cycles with 125-point trails each (~2000
    /// walls), as late in a long human round. Not run by default:
    /// `cargo test --release -p server bench_dense -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_dense_round() {
        let mut g = Game::new(0, 10, 7);
        g.phase = phase::PLAYING;
        for i in 0..16u8 {
            let (x0, x1) = (i as f32 * 30.0 + 8.0, i as f32 * 30.0 + 26.0);
            let mut trail = Vec::new();
            for k in 0..62 {
                let y = 5.0 + k as f32 * 3.0;
                if k % 2 == 0 { trail.extend([(x0, y), (x1, y)]); } else { trail.extend([(x1, y), (x0, y)]); }
            }
            let (hx, hy) = *trail.last().unwrap();
            trail.push((hx, hy + 1.0));
            g.cycles.push(cycle(i, hx, hy + 3.0, 1, trail, true));
            g.players[i as usize] = Some(Player { name: "b".into(), color: [9, 9, 9], score: 0, kills: 0, deaths: 0, bot: true });
        }
        let segs: usize = g.cycles.iter().map(|c| c.trail.len()).sum();
        let ticks = 300;
        let t = std::time::Instant::now();
        for _ in 0..ticks {
            g.tick += 1;
            g.simulate();
            g.bots_think();
        }
        let alive = g.cycles.iter().filter(|c| c.alive).count();
        println!("dense round: {segs} wall points, {alive}/16 alive after: {:.1} us per tick", t.elapsed().as_secs_f64() * 1e6 / ticks as f64);
    }

    /// Driving into the closed end of someone's U-shaped trail counts as boxed;
    /// hitting a lone wall in the open doesn't.
    #[test]
    fn crash_inside_a_pocket_is_boxed() {
        let mut g = Game::new(0, 10, 7);
        g.phase = phase::PLAYING;
        g.cycles.push(cycle(0, 105.0, 5.0, 0, vec![(90.0, 5.0)], true));
        g.cycles.push(cycle(1, 100.0, 8.0, 2, vec![(100.0, 2.0), (110.0, 2.0), (110.0, 8.0), (100.0, 8.0)], false));
        g.cycles.push(cycle(2, 300.0, 300.0, 1, vec![(300.0, 290.0)], true));
        let mut seen = Vec::new();
        for _ in 0..(STUCK_GRACE_TICKS * 3) {
            g.tick += 1;
            g.simulate();
            if !g.cycles[0].alive && seen.is_empty() {
                let c = &g.cycles[0];
                seen.push(g.boxed(0, c.x, c.y, c.dir));
            }
        }
        assert_eq!(seen, [true]);
        let c = &g.cycles[2];
        assert!(!g.boxed(2, c.x, c.y, c.dir));
    }
}
