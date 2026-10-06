//! Shared constants, wire protocol and wall geometry for server and client.

pub const TICK_HZ: f64 = 60.0;
pub const ARENA: f32 = 500.0;
pub const BASE_SPEED: f32 = 45.0; // units / second
pub const MAX_SPEED: f32 = 140.0;
pub const GRIND_RANGE: f32 = 5.0; // riding this close to a wall accelerates you
pub const GRIND_ACCEL: f32 = 75.0;
pub const SPEED_DECAY: f32 = 0.45; // fraction of excess speed lost per second
pub const BRAKE_SPEED: f32 = 18.0; // floor while braking
pub const BRAKE_DECEL: f32 = 90.0;
pub const BRAKE_DRAIN: f32 = 1.0 / 1.5; // a full tank lasts 1.5 s
pub const BRAKE_RECHARGE: f32 = 1.0 / 5.0;
pub const STUCK_GRACE_TICKS: u32 = 9; // time pressed against a wall before you derez
pub const MAX_REWIND_TICKS: f64 = 15.0; // lag compensation window for turns
pub const WALL_HEIGHT: f32 = 2.2;
pub const WALL_LINGER_TICKS: u64 = 120; // walls of the dead vanish after this
pub const MAX_PLAYERS: usize = 16;
pub const ROUNDS: u8 = 10; // default match length
pub const NO_ID: u8 = 255;
/// Killer id reported when the sudden-death zone takes someone.
pub const ZONE_ID: u8 = 254;
/// Sudden death: the zone starts closing this long into a round...
pub const ZONE_START_TICKS: u64 = 60 * 60;
/// ...or this long after the last human in the round is out.
pub const ZONE_AFTER_HUMANS_TICKS: u64 = 3 * 60;
pub const ZONE_SPEED: f32 = 7.0; // units / second, per side
/// Zone speed once every human is out, so nobody waits long for the next round.
pub const ZONE_FAST_SPEED: f32 = 30.0;
/// The victim had walls close ahead, left and right: they were boxed in.
pub const DEATH_BOXED: u8 = 1;
/// Boxed means walls on both sides within this distance.
pub const BOX_RANGE: f32 = 9.0;

pub mod score {
    pub const KILL: i16 = 2;
    pub const DEATH: i16 = -1;
    pub const SUICIDE: i16 = -3;
    pub const WIN: i16 = 3;
    /// Extra for trapping someone so they had nowhere left to go.
    pub const BOX: i16 = 1;
}

/// 0:+x 1:+y 2:-x 3:-y. Left turn is counter-clockwise.
pub const DIRS: [(f32, f32); 4] = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)];

#[inline]
pub fn turn(dir: u8, left: bool) -> u8 {
    if left { (dir + 1) & 3 } else { (dir + 3) & 3 }
}

pub mod msg {
    // server -> client
    pub const WELCOME: u8 = 1; // [your_id u8]
    /// Live cycles only (the dead hold still where DEATH left them).
    pub const SNAPSHOT: u8 = 2; // [tick u32][zone inset u16 pos][n u8] n*CYCLE
    // CYCLE = [id u8][flags u8 | dir << 4][x u16 pos][y u16 pos][speed u16 (1/256)][brake u8 (0..255 energy)]
    // "pos" = 1/POS_SCALE units (see `pos_q`): 9 bytes a cycle instead of 16.
    pub const TURN: u8 = 3; // [id u8][seq u8][dir u8][x f32][y f32][tick f64]
    pub const DEATH: u8 = 4; // [id u8][killer u8 (= id: own wall, NO_ID: rim)][x f32][y f32][flags u8: DEATH_BOXED]
    pub const WALLS_GONE: u8 = 5; // [id u8]
    pub const FULL: u8 = 6; // [tick u32][phase u8][phase_end u32][round u8][rounds u8][n u8] n*[CYCLE][npts u16][pts f32*2]
    pub const ROSTER: u8 = 7; // [n u8] n*[id u8][r g b u8][bot u8][score i16][kills u16][deaths u16][len u8][name]
    pub const PHASE: u8 = 8; // [phase u8][phase_end u32][winner u8][round u8][rounds u8]
    pub const PONG: u8 = 9; // [client_time f64][server_time f64 (ticks)]
    // Lobbies. Until a connection enters one it only sees these and PONG.
    // LOBBY = [len u8][code][humans u8][bots u8][watchers u8][round u8][rounds u8][flags u8][len u8][name]
    pub const LOBBIES: u8 = 10; // [n u8] n*LOBBY (public ones only)
    pub const ENTERED: u8 = 11; // LOBBY; followed by WELCOME, ROSTER and FULL from its game
    pub const LOBBY_ERR: u8 = 12; // [lobby_err u8]
    // client -> server
    pub const C_JOIN: u8 = 0x10; // [r g b u8][len u8][name]; again to change name/colour
    pub const C_TURN: u8 = 0x11; // [seq u8][left u8][time f64 (ticks)]
    pub const C_PING: u8 = 0x12; // [client_time f64]
    pub const C_BRAKE: u8 = 0x13; // [on u8]
    pub const C_LIST: u8 = 0x14;
    /// [bots u8][rounds u8][private u8][len u8][name][len u8][password (empty: none)]
    pub const C_CREATE: u8 = 0x15;
    pub const C_ENTER: u8 = 0x16; // [len u8][code][len u8][password]
    pub const C_LEAVE: u8 = 0x17;
}

pub mod lobby {
    /// LOBBY flags.
    pub const LOCKED: u8 = 1;
    pub const PRIVATE: u8 = 2;
    pub const MAIN: u8 = 4;
    /// LOBBY_ERR codes.
    pub const NOT_FOUND: u8 = 1;
    pub const BAD_PASSWORD: u8 = 2;
    pub const TOO_MANY: u8 = 3;
    pub const SLOW_DOWN: u8 = 4;
    pub const INVALID: u8 = 5;
    pub const FULL: u8 = 6;
    pub const MAX_NAME: usize = 24;
    pub const MAX_PASSWORD: usize = 32;
    pub const MAX_ROUNDS: u8 = 20;
}

pub mod phase {
    pub const WAITING: u8 = 0;
    pub const COUNTDOWN: u8 = 1;
    pub const PLAYING: u8 = 2;
    pub const ROUND_OVER: u8 = 3;
    pub const MATCH_OVER: u8 = 4;
}

/// Positions in SNAPSHOT / FULL travel as u16 in 1/POS_SCALE units:
/// 0.008-unit steps, up to 512 units (the arena is 500).
pub const POS_SCALE: f32 = 128.0;
pub fn pos_q(v: f32) -> u16 {
    (v * POS_SCALE).round().clamp(0.0, u16::MAX as f32) as u16
}
pub fn pos_dq(v: u16) -> f32 {
    v as f32 / POS_SCALE
}

pub const FLAG_ALIVE: u8 = 1;
pub const FLAG_STUCK: u8 = 2;
pub const FLAG_BRAKING: u8 = 4;

/// An axis-aligned wall segment.
#[derive(Clone, Copy, Debug)]
pub struct Seg {
    pub ax: f32,
    pub ay: f32,
    pub bx: f32,
    pub by: f32,
    pub owner: u8,
    /// True for the segment that is still growing behind its cycle.
    pub current: bool,
}

const EPS: f32 = 1e-4;

#[inline]
fn minmax(a: f32, b: f32) -> (f32, f32) {
    if a < b { (a, b) } else { (b, a) }
}

/// Earliest point where the axis-aligned path p0->p1 enters wall `s`,
/// as a fraction t in (0, 1]. Touching the wall exactly at p0 does not count.
pub fn path_hit(x0: f32, y0: f32, x1: f32, y1: f32, s: &Seg) -> Option<f32> {
    let horiz = (x1 - x0).abs() >= (y1 - y0).abs();
    // "u" runs along the path, "v" across it.
    let (u0, u1, v, wau, wav, wbu, wbv) = if horiz {
        (x0, x1, y0, s.ax, s.ay, s.bx, s.by)
    } else {
        (y0, y1, x0, s.ay, s.ax, s.by, s.bx)
    };
    let len = u1 - u0;
    if len.abs() < 1e-7 {
        return None;
    }
    if (wav - wbv).abs() < 1e-6 {
        // Wall parallel to the path: only collinear walls matter.
        if (wav - v).abs() > EPS {
            return None;
        }
        let (lo, hi) = minmax(wau, wbu);
        if u0 > lo + EPS && u0 < hi - EPS {
            return Some(1e-6); // already inside it
        }
        if (u0 - lo).abs() <= EPS {
            return if len > 0.0 && hi - lo > EPS { Some(1e-6) } else { None };
        }
        if (u0 - hi).abs() <= EPS {
            return if len < 0.0 && hi - lo > EPS { Some(1e-6) } else { None };
        }
        let entry = if len > 0.0 { lo } else { hi };
        let t = (entry - u0) / len;
        if t > 0.0 && t <= 1.0 { Some(t) } else { None }
    } else {
        // Perpendicular wall at u = wau.
        let along = (wau - u0) * len.signum();
        if along <= EPS || along > len.abs() {
            return None;
        }
        let (lo, hi) = minmax(wav, wbv);
        if v < lo - EPS || v > hi + EPS {
            return None;
        }
        Some(along / len.abs())
    }
}

/// First wall hit along p0->p1 skipping walls rejected by `skip`; returns (t, owner).
pub fn first_hit(x0: f32, y0: f32, x1: f32, y1: f32, segs: &[Seg], skip: impl Fn(&Seg) -> bool) -> Option<(f32, u8)> {
    let mut best: Option<(f32, u8)> = None;
    for s in segs {
        if skip(s) {
            continue;
        }
        if let Some(t) = path_hit(x0, y0, x1, y1, s) {
            if best.map_or(true, |(bt, _)| t < bt) {
                best = Some((t, s.owner));
            }
        }
    }
    best
}

/// Distance to the nearest wall from (x,y) heading `dir`, capped at `max`.
pub fn raycast(x: f32, y: f32, dir: u8, max: f32, segs: &[Seg], skip: impl Fn(&Seg) -> bool) -> f32 {
    let (dx, dy) = DIRS[dir as usize];
    match first_hit(x, y, x + dx * max, y + dy * max, segs, skip) {
        Some((t, _)) => t * max,
        None => max,
    }
}

/// Uniform grid over the arena so wall queries only test nearby walls instead
/// of every wall. Results match the linear `first_hit` / `raycast` exactly,
/// including which wall wins a tie (the earliest in `segs`).
pub struct Grid {
    cells: Vec<Vec<u32>>,
    /// Per wall: the query stamp that last tested it, so walls spanning several
    /// cells are tested once per query.
    mark: Vec<u32>,
    stamp: u32,
}

const CELL: f32 = 16.0;
const GN: usize = (ARENA / CELL) as usize + 2;
/// Walls are filed into every cell within this distance (> EPS), so touching
/// and collinear cases near cell borders are never missed.
const MARGIN: f32 = 0.05;

#[inline]
fn cell_of(v: f32) -> usize {
    // `as usize` saturates (negatives become 0) and truncates, which is floor
    // for everything that survives the clamp.
    ((v * (1.0 / CELL)) as usize).min(GN - 1)
}

impl Default for Grid {
    fn default() -> Grid {
        Grid { cells: vec![Vec::new(); GN * GN], mark: Vec::new(), stamp: 0 }
    }
}

impl Grid {
    /// Files `segs`; queries must then be given the same slice.
    pub fn build(&mut self, segs: &[Seg]) {
        for c in self.cells.iter_mut() {
            c.clear();
        }
        self.mark.clear();
        self.stamp = 0;
        for s in segs {
            self.push(s);
        }
    }

    /// Files one more wall: the next index after those already filed.
    pub fn push(&mut self, s: &Seg) {
        let i = self.mark.len() as u32;
        self.mark.push(0);
        let (x0, x1) = minmax(s.ax, s.bx);
        let (y0, y1) = minmax(s.ay, s.by);
        for cy in cell_of(y0 - MARGIN)..=cell_of(y1 + MARGIN) {
            for cx in cell_of(x0 - MARGIN)..=cell_of(x1 + MARGIN) {
                self.cells[cy * GN + cx].push(i);
            }
        }
    }

    fn next_stamp(&mut self) -> u32 {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.mark.fill(0);
            self.stamp = 1;
        }
        self.stamp
    }

    /// `first_hit` for an axis-aligned path, walking the cells it crosses in
    /// order and stopping once nothing further along can be closer.
    pub fn first_hit(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, segs: &[Seg], skip: impl Fn(&Seg) -> bool) -> Option<(f32, u8)> {
        self.first_hit_at(x0, y0, x1, y1, segs, skip).map(|(t, i)| (t, segs[i as usize].owner))
    }

    /// `first_hit`, returning the wall's index instead of its owner.
    fn first_hit_at(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, segs: &[Seg], skip: impl Fn(&Seg) -> bool) -> Option<(f32, u32)> {
        let st = self.next_stamp();
        let horiz = (x1 - x0).abs() >= (y1 - y0).abs();
        let (u0, u1, v) = if horiz { (x0, x1, y0) } else { (y0, y1, x0) };
        let len = (u1 - u0).abs();
        let (k0, k1) = (cell_of(u0), cell_of(u1));
        let fwd = u1 >= u0;
        let (r0, r1) = (cell_of(v - MARGIN), cell_of(v + MARGIN));
        let mut best: Option<(f32, u32)> = None;
        let mut k = k0;
        loop {
            for r in r0..=r1 {
                let cell = if horiz { r * GN + k } else { k * GN + r };
                for &i in &self.cells[cell] {
                    if self.mark[i as usize] == st {
                        continue;
                    }
                    self.mark[i as usize] = st;
                    let s = &segs[i as usize];
                    if skip(s) {
                        continue;
                    }
                    if let Some(t) = path_hit(x0, y0, x1, y1, s) {
                        if best.map_or(true, |(bt, bi)| t < bt || (t == bt && i < bi)) {
                            best = Some((t, i));
                        }
                    }
                }
            }
            if k == k1 {
                break;
            }
            // Walls only in later cells start at least MARGIN past this cell's
            // far edge, so a hit no further than that edge can't be beaten.
            let edge = if fwd { (k + 1) as f32 * CELL - u0 } else { u0 - k as f32 * CELL };
            if best.is_some_and(|(t, _)| t * len <= edge) {
                break;
            }
            k = if fwd { k + 1 } else { k - 1 };
        }
        best
    }

    /// `raycast` using the grid.
    pub fn raycast(&mut self, x: f32, y: f32, dir: u8, max: f32, segs: &[Seg], skip: impl Fn(&Seg) -> bool) -> f32 {
        let (dx, dy) = DIRS[dir as usize];
        match self.first_hit(x, y, x + dx * max, y + dy * max, segs, skip) {
            Some((t, _)) => t * max,
            None => max,
        }
    }

    /// Calls `f` once for every wall filed within `r` of (x, y) (a superset of
    /// the walls actually that close).
    pub fn near(&mut self, x: f32, y: f32, r: f32, segs: &[Seg], mut f: impl FnMut(&Seg)) {
        let st = self.next_stamp();
        for cy in cell_of(y - r)..=cell_of(y + r) {
            for cx in cell_of(x - r)..=cell_of(x + r) {
                for &i in &self.cells[cy * GN + cx] {
                    if self.mark[i as usize] != st {
                        self.mark[i as usize] = st;
                        f(&segs[i as usize]);
                    }
                }
            }
        }
    }
}

/// Every wall in play, split so per-tick upkeep is tiny: finished walls never
/// move, so they're filed in a `Grid` once; the few that change every tick
/// (each cycle's growing segment, the zone) are checked directly.
/// A tie between a finished and a moving wall goes to the finished one.
pub struct World {
    fixed: Vec<Seg>,
    grid: Grid,
    pub moving: Vec<Seg>,
}

impl Default for World {
    fn default() -> World {
        let mut w = World { fixed: Vec::new(), grid: Grid::default(), moving: Vec::new() };
        w.reset();
        w
    }
}

impl World {
    /// Back to just the rim.
    pub fn reset(&mut self) {
        self.fixed.clear();
        self.fixed.extend_from_slice(&rim());
        self.grid.build(&self.fixed);
        self.moving.clear();
    }

    pub fn add_fixed(&mut self, s: Seg) {
        self.grid.push(&s);
        self.fixed.push(s);
    }

    pub fn first_hit(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, skip: impl Fn(&Seg) -> bool) -> Option<(f32, u8)> {
        let mut best = self.grid.first_hit_at(x0, y0, x1, y1, &self.fixed, &skip).map(|(t, i)| (t, self.fixed[i as usize].owner));
        for s in &self.moving {
            if skip(s) {
                continue;
            }
            if let Some(t) = path_hit(x0, y0, x1, y1, s) {
                if best.map_or(true, |(bt, _)| t < bt) {
                    best = Some((t, s.owner));
                }
            }
        }
        best
    }

    pub fn raycast(&mut self, x: f32, y: f32, dir: u8, max: f32, skip: impl Fn(&Seg) -> bool) -> f32 {
        let (dx, dy) = DIRS[dir as usize];
        match self.first_hit(x, y, x + dx * max, y + dy * max, skip) {
            Some((t, _)) => t * max,
            None => max,
        }
    }

    /// Every wall that might lie within `r` of (x, y), each once.
    pub fn near(&mut self, x: f32, y: f32, r: f32, mut f: impl FnMut(&Seg)) {
        self.grid.near(x, y, r, &self.fixed, &mut f);
        for s in &self.moving {
            f(s);
        }
    }
}

/// The four arena boundary walls.
pub fn rim() -> [Seg; 4] {
    square(0.0, NO_ID)
}

/// A square of walls inset `d` from the arena edge.
pub fn square(d: f32, owner: u8) -> [Seg; 4] {
    let (lo, hi) = (d, ARENA - d);
    let s = |ax, ay, bx, by| Seg { ax, ay, bx, by, owner, current: false };
    [s(lo, lo, hi, lo), s(hi, lo, hi, hi), s(hi, hi, lo, hi), s(lo, hi, lo, lo)]
}

/// Little-endian message writer.
#[derive(Default)]
pub struct W(pub Vec<u8>);

impl W {
    pub fn new(kind: u8) -> W {
        let mut v = Vec::with_capacity(64);
        v.push(kind);
        W(v)
    }
    /// Takes the finished message out of the builder.
    pub fn done(&mut self) -> Vec<u8> { std::mem::take(&mut self.0) }
    pub fn u8(&mut self, v: u8) -> &mut Self { self.0.push(v); self }
    pub fn u16(&mut self, v: u16) -> &mut Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn i16(&mut self, v: i16) -> &mut Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn u32(&mut self, v: u32) -> &mut Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn f32(&mut self, v: f32) -> &mut Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn f64(&mut self, v: f64) -> &mut Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self { self.0.extend_from_slice(v); self }
}

/// Little-endian message reader; reads past the end yield zeros and clear `ok`.
pub struct R<'a> {
    b: &'a [u8],
    p: usize,
    pub ok: bool,
}

impl<'a> R<'a> {
    pub fn new(b: &'a [u8]) -> R<'a> { R { b, p: 0, ok: true } }
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        if self.p + N <= self.b.len() {
            out.copy_from_slice(&self.b[self.p..self.p + N]);
            self.p += N;
        } else {
            self.ok = false;
        }
        out
    }
    pub fn u8(&mut self) -> u8 { self.take::<1>()[0] }
    pub fn u16(&mut self) -> u16 { u16::from_le_bytes(self.take()) }
    pub fn i16(&mut self) -> i16 { i16::from_le_bytes(self.take()) }
    pub fn u32(&mut self) -> u32 { u32::from_le_bytes(self.take()) }
    pub fn f32(&mut self) -> f32 { f32::from_le_bytes(self.take()) }
    pub fn f64(&mut self) -> f64 { f64::from_le_bytes(self.take()) }
    pub fn bytes(&mut self, n: usize) -> &'a [u8] {
        if self.p + n <= self.b.len() {
            let s = &self.b[self.p..self.p + n];
            self.p += n;
            s
        } else {
            self.ok = false;
            &[]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built incrementally, the world answers like a linear scan over the same
    /// walls (ties aside, which can only differ in the owner reported).
    #[test]
    fn world_matches_linear_scan() {
        let mut world = World::default();
        let mut all: Vec<Seg> = rim().to_vec();
        let mut rng = 99u64;
        let mut rand = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32
        };
        let (mut x, mut y) = (250.0f32, 250.0f32);
        for step in 0..400 {
            let d = (rand() * 4.0) as usize % 4;
            let l = rand() * 30.0;
            let (nx, ny) = ((x + DIRS[d].0 * l).clamp(0.0, ARENA), (y + DIRS[d].1 * l).clamp(0.0, ARENA));
            let s = Seg { ax: x, ay: y, bx: nx, by: ny, owner: (step % 5) as u8, current: false };
            world.add_fixed(s);
            all.push(s);
            (x, y) = (nx, ny);
            world.moving = vec![Seg { ax: x, ay: y, bx: x + 7.0, by: y, owner: 9, current: true }, square(40.0, ZONE_ID)[step % 4]];
            let every: Vec<Seg> = all.iter().chain(&world.moving).copied().collect();
            for _ in 0..20 {
                let (qx, qy, dir, max) = (rand() * ARENA, rand() * ARENA, (rand() * 4.0) as u8 % 4, rand() * 300.0);
                let a = raycast(qx, qy, dir, max, &every, |_| false);
                let b = world.raycast(qx, qy, dir, max, |_| false);
                assert_eq!(a, b, "step {step}: ({qx}, {qy}) dir {dir} max {max}");
            }
        }
    }

    /// The grid must give exactly the linear scan's answers, ties included.
    #[test]
    fn grid_matches_linear_scan() {
        let mut rng = 0x2545f4914f6cdd1du64;
        let mut rand = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut grid = Grid::default();
        for round in 0..200 {
            let mut segs: Vec<Seg> = rim().to_vec();
            // Random axis-aligned trails, snapped to a coarse lattice so walls
            // often touch, overlap, share endpoints and sit on cell borders.
            for owner in 0..8u8 {
                let snap = |v: f32| (v * 0.5).round() * 2.0;
                let (mut x, mut y) = (snap(rand() * ARENA), snap(rand() * ARENA));
                for _ in 0..40 {
                    let d = (rand() * 4.0) as usize % 4;
                    let l = snap(rand() * 40.0);
                    let (nx, ny) = ((x + DIRS[d].0 * l).clamp(0.0, ARENA), (y + DIRS[d].1 * l).clamp(0.0, ARENA));
                    segs.push(Seg { ax: x, ay: y, bx: nx, by: ny, owner, current: owner == 3 });
                    (x, y) = (nx, ny);
                }
            }
            grid.build(&segs);
            for q in 0..300 {
                let snap = |v: f32| if q % 2 == 0 { (v * 0.5).round() * 2.0 } else { v };
                let (x, y) = (snap(rand() * ARENA), snap(rand() * ARENA));
                let dir = (rand() * 4.0) as u8 % 4;
                let max = if q % 3 == 0 { 2.5 } else { rand() * 320.0 };
                let skip = |s: &Seg| s.current;
                let (dx, dy) = DIRS[dir as usize];
                let a = first_hit(x, y, x + dx * max, y + dy * max, &segs, skip);
                let b = grid.first_hit(x, y, x + dx * max, y + dy * max, &segs, skip);
                assert_eq!(a, b, "round {round} query {q}: ({x}, {y}) dir {dir} max {max}");
            }
        }
    }
    fn seg(ax: f32, ay: f32, bx: f32, by: f32) -> Seg { Seg { ax, ay, bx, by, owner: 0, current: false } }

    #[test]
    fn perpendicular() {
        let w = seg(5.0, -1.0, 5.0, 1.0);
        assert_eq!(path_hit(0.0, 0.0, 10.0, 0.0, &w), Some(0.5));
        assert_eq!(path_hit(10.0, 0.0, 0.0, 0.0, &w), Some(0.5));
        assert_eq!(path_hit(0.0, 2.0, 10.0, 2.0, &w), None);
        assert_eq!(path_hit(5.0, 0.0, 10.0, 0.0, &w), None); // starts on it, moving away
    }

    #[test]
    fn collinear() {
        let w = seg(5.0, 0.0, 8.0, 0.0);
        assert_eq!(path_hit(0.0, 0.0, 10.0, 0.0, &w), Some(0.5));
        assert!(path_hit(6.0, 0.0, 7.0, 0.0, &w).is_some());
        assert_eq!(path_hit(8.0, 0.0, 10.0, 0.0, &w), None);
        assert!(path_hit(8.0, 0.0, 7.0, 0.0, &w).is_some());
    }

    #[test]
    fn ray() {
        let r = rim();
        let d = raycast(10.0, 10.0, 0, 1000.0, &r, |_| false);
        assert!((d - (ARENA - 10.0)).abs() < 1e-3);
    }
}
