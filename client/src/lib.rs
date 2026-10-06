//! Retro Cycles web client. Pure Rust compiled to wasm32 with no dependencies.
//!
//! JS forwards socket bytes and input, then each frame calls `frame()`, which
//! advances prediction and writes every vertex of the scene into one buffer the
//! JS shim uploads with a single `bufferSubData` and draws in a few ranges.

mod render;

use common::*;
use render::{Mat4, Vert};
use std::f32::consts::{FRAC_PI_2, TAU};
use std::ptr::addr_of_mut;

#[link(wasm_import_module = "env")]
extern "C" {
    fn ws_send(ptr: *const u8, len: usize);
    fn hud(slot: u32, ptr: *const u8, len: usize);
    fn sfx(kind: u32, a: f32, b: f32);
    fn console_log(ptr: *const u8, len: usize);
}

fn send(bytes: &[u8]) {
    unsafe { ws_send(bytes.as_ptr(), bytes.len()) }
}

fn play(kind: u32, a: f32, b: f32) {
    unsafe { sfx(kind, a, b) }
}

mod snd {
    pub const ENGINE: u32 = 0; // a = speed 0..1 (negative: silent), b = braking
    pub const TURN: u32 = 1;
    pub const DEREZ: u32 = 2; // a = volume
    pub const HIT: u32 = 3;
    pub const BEEP: u32 = 4;
    pub const GO: u32 = 5;
    pub const RESULT: u32 = 6; // a = 1 won, 0 lost
    pub const GRIND: u32 = 7; // a = intensity 0..1, b = speed 0..1 (continuous)
    pub const CLOSE: u32 = 8;
    pub const SAVE: u32 = 9;
    pub const STING: u32 = 10; // a = tier
}

mod slot {
    pub const BOARD: u32 = 0;
    pub const CENTER: u32 = 1;
    pub const STATS: u32 = 2;
    pub const HINT: u32 = 3;
    pub const HIT: u32 = 4;
    pub const FEED: u32 = 5;
    pub const METERS: u32 = 6;
    pub const ROUND: u32 = 7;
    /// "rrggbb alpha": screen-edge glow in the rider's colour.
    pub const EDGE: u32 = 8;
    /// "style\ttext": announcer callout (style: kill, spree, save, close, voice).
    pub const CALLOUT: u32 = 9;
    /// "fx fy\ttext": text floating up from a point on screen (fractions of the viewport).
    pub const POP: u32 = 10;
}

/// Kill-streak callouts, Unreal Tournament style.
const STREAKS: [(u32, &str); 5] = [(3, "KILLING SPREE"), (5, "RAMPAGE"), (7, "DOMINATING"), (10, "UNSTOPPABLE"), (15, "GODLIKE")];

const CAM_NAMES: [&str; 4] = ["chase", "cockpit", "overhead", "arena"];

pub(crate) struct Cyc {
    pub id: u8,
    pub alive: bool,
    pub braking: bool,
    pub brake: f32,
    pub dir: u8,
    pub x: f32,
    pub y: f32,
    pub speed: f32,
    pub stuck: bool,
    /// Server time (ticks) at which (x, y) was true.
    pub t: f64,
    pub trail: Vec<(f32, f32)>,
    pub walls: bool,
    /// Local ms when walls started fading out.
    pub gone_ms: Option<f64>,
    pub death_ms: f64,
    // Derived each frame.
    pub hx: f32,
    pub hy: f32,
    pub hdir: u8,
    /// Smoothed heading of the model, so turns sweep instead of snapping.
    pub vis_yaw: f32,
    vis_init: bool,
}

struct Pending {
    seq: u8,
    t: f64,
    dir: u8,
}

pub(crate) struct Player {
    pub id: u8,
    pub color: [f32; 3],
    pub bot: bool,
    pub score: i16,
    pub kills: u16,
    pub deaths: u16,
    pub name: String,
}

pub(crate) struct Particle {
    pub p: [f32; 3],
    pub v: [f32; 3],
    pub life: f32,
    pub max: f32,
    pub col: [f32; 3],
}

pub(crate) struct Ring {
    pub x: f32,
    pub y: f32,
    pub age: f32,
    pub col: [f32; 3],
}

struct FeedLine {
    text: String,
    until: f64,
}

pub(crate) struct State {
    connected: bool,
    pub my_id: u8,
    phase: u8,
    phase_end: f64,
    round: u8,
    rounds: u8,
    /// Sudden-death zone inset (0 = not active).
    pub zone: f32,
    winner: u8,
    pub cycles: Vec<Cyc>,
    pub players: Vec<Player>,
    // Clock sync: server_ticks = local_ms * HZ / 1000 + offset.
    offset: f64,
    clock_ok: bool,
    rtt: f64,
    best_rtt: f64,
    pings: u32,
    next_ping: f64,
    pending: Vec<Pending>,
    /// Extra trail points from unacknowledged local turns.
    pub my_extra: Vec<(f32, f32)>,
    seq: u8,
    braking: bool,
    cam_mode: u8,
    cam_yaw: f32,
    /// Jump the camera straight to its target heading (new round).
    cam_snap: bool,
    cam_eye: [f32; 3],
    follow: u8,
    pub minimap: bool,
    pub particles: Vec<Particle>,
    pub rings: Vec<Ring>,
    feed: Vec<FeedLine>,
    pub verts: Vec<Vert>,
    pub uniforms: [f32; 32],
    segs: Vec<Seg>,
    inbox: Vec<u8>,
    now: f64,
    last_ms: f64,
    fps: f32,
    fps_frames: u32,
    fps_t0: f64,
    next_hud: f64,
    hud_cache: [String; 11],
    last_count: i32,
    rng: u32,
    killed_by: u8,
    killed_boxed: bool,
    // Feel: camera shake amplitude, FOV punch, smoothed grind intensity.
    shake: f32,
    fov_kick: f32,
    grind: f32,
    spark_acc: f32,
    close_until: f64,
    // Announcer bookkeeping for the local player.
    streak: u32,
    multi: u32,
    last_kill_ms: f64,
    nemesis: u8,
    first_blood: bool,
}

static mut STATE: Option<State> = None;

fn st() -> &'static mut State {
    // Single-threaded wasm: the only access path to STATE.
    unsafe { (*addr_of_mut!(STATE)).get_or_insert_with(State::new) }
}

pub(crate) fn color_of(players: &[Player], id: u8) -> [f32; 3] {
    players.iter().find(|p| p.id == id).map_or([0.7, 0.7, 0.7], |p| p.color)
}

fn hex(c: [f32; 3]) -> String {
    c.iter().map(|v| format!("{:02x}", (v * 255.0) as u8)).collect()
}

fn wrap(a: f32) -> f32 {
    a - TAU * (a / TAU).round()
}

impl State {
    fn new() -> State {
        State {
            connected: false,
            my_id: NO_ID,
            phase: phase::WAITING,
            phase_end: 0.0,
            round: 1,
            rounds: ROUNDS,
            zone: 0.0,
            winner: NO_ID,
            cycles: Vec::new(),
            players: Vec::new(),
            offset: 0.0,
            clock_ok: false,
            rtt: 0.0,
            best_rtt: f64::MAX,
            pings: 0,
            next_ping: 0.0,
            pending: Vec::new(),
            my_extra: Vec::new(),
            seq: 0,
            braking: false,
            cam_mode: 0,
            cam_yaw: 0.0,
            cam_snap: true,
            cam_eye: [0.0; 3],
            follow: NO_ID,
            minimap: true,
            particles: Vec::new(),
            rings: Vec::new(),
            feed: Vec::new(),
            verts: Vec::with_capacity(1 << 16),
            uniforms: [0.0; 32],
            segs: Vec::new(),
            inbox: Vec::new(),
            now: 0.0,
            last_ms: 0.0,
            fps: 0.0,
            fps_frames: 0,
            fps_t0: 0.0,
            next_hud: 0.0,
            hud_cache: Default::default(),
            last_count: 0,
            rng: 0x9e3779b9,
            killed_by: NO_ID,
            killed_boxed: false,
            shake: 0.0,
            fov_kick: 0.0,
            grind: 0.0,
            spark_acc: 0.0,
            close_until: 0.0,
            streak: 0,
            multi: 0,
            last_kill_ms: 0.0,
            nemesis: NO_ID,
            first_blood: false,
        }
    }

    pub fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    fn present(&self, ms: f64) -> f64 {
        ms * TICK_HZ / 1000.0 + self.offset
    }

    fn name(&self, id: u8) -> String {
        self.players.iter().find(|p| p.id == id).map_or("Unknown".to_string(), |p| p.name.clone())
    }

    fn cycle(&mut self, id: u8) -> &mut Cyc {
        if let Some(i) = self.cycles.iter().position(|c| c.id == id) {
            return &mut self.cycles[i];
        }
        self.cycles.push(Cyc {
            id,
            alive: false,
            braking: false,
            brake: 1.0,
            dir: 0,
            x: 0.0,
            y: 0.0,
            speed: 0.0,
            stuck: false,
            t: 0.0,
            trail: Vec::new(),
            walls: true,
            gone_ms: None,
            death_ms: 0.0,
            hx: 0.0,
            hy: 0.0,
            hdir: 0,
            vis_yaw: 0.0,
            vis_init: false,
        });
        self.cycles.last_mut().unwrap()
    }

    fn read_cycle(&mut self, r: &mut R, t: f64) -> u8 {
        let (id, flags) = (r.u8(), r.u8());
        let dir = flags >> 4;
        let (x, y) = (pos_dq(r.u16()), pos_dq(r.u16()));
        let (speed, brake) = (r.u16() as f32 / 256.0, r.u8());
        let c = self.cycle(id);
        let stuck = flags & FLAG_STUCK != 0;
        // Was against a wall, now driving free again: a last-moment escape.
        let saved = c.stuck && !stuck && flags & FLAG_ALIVE != 0;
        c.stuck = stuck;
        c.alive = flags & FLAG_ALIVE != 0;
        c.braking = flags & FLAG_BRAKING != 0;
        c.brake = brake as f32 / 255.0;
        c.dir = dir & 3;
        c.x = x;
        c.y = y;
        c.speed = speed;
        c.t = t;
        if saved && id == self.my_id && self.phase == phase::PLAYING {
            self.callout("save", "SAVED");
            play(snd::SAVE, 0.0, 0.0);
            self.fov_kick = self.fov_kick.max(0.08);
        }
        id
    }

    fn on_message(&mut self, data: &[u8], now: f64) {
        let mut r = R::new(data);
        match r.u8() {
            msg::WELCOME => {
                self.my_id = r.u8();
                self.pending.clear();
            }
            msg::SNAPSHOT => {
                let tick = r.u32() as f64;
                if !self.clock_ok {
                    self.offset = tick - now * TICK_HZ / 1000.0;
                }
                self.zone = pos_dq(r.u16());
                let n = r.u8();
                for _ in 0..n {
                    self.read_cycle(&mut r, tick);
                }
            }
            msg::TURN => {
                let (id, seq, dir) = (r.u8(), r.u8(), r.u8());
                let (x, y, t) = (r.f32(), r.f32(), r.f64());
                let c = self.cycle(id);
                let last = c.trail.last().copied();
                if last.map_or(true, |(lx, ly)| (lx - x).abs() + (ly - y).abs() > 1e-3) {
                    c.trail.push((x, y));
                }
                c.dir = dir & 3;
                c.x = x;
                c.y = y;
                c.t = t;
                if id == self.my_id {
                    if let Some(i) = self.pending.iter().position(|p| p.seq == seq) {
                        self.pending.drain(..=i);
                    }
                }
            }
            msg::DEATH => {
                let (id, killer, x, y) = (r.u8(), r.u8(), r.f32(), r.f32());
                let boxed = r.u8() & DEATH_BOXED != 0;
                let c = self.cycle(id);
                c.alive = false;
                c.x = x;
                c.y = y;
                c.death_ms = now;
                let me = self.my_id;
                if id == me {
                    self.pending.clear();
                    self.killed_by = killer;
                    self.killed_boxed = boxed;
                    self.streak = 0;
                    self.multi = 0;
                    self.shake = self.shake.max(1.2);
                    if killer != me && (killer as usize) < MAX_PLAYERS {
                        self.nemesis = killer;
                    }
                }
                let col = color_of(&self.players, id);
                self.explode(x, y, col, 140);
                let dist = ((self.cam_eye[0] - x).powi(2) + (self.cam_eye[2] + y).powi(2)).sqrt();
                play(snd::DEREZ, (1.0 - dist / 400.0).clamp(0.08, 1.0), 0.0);
                self.shake = self.shake.max(0.5 * (1.0 - dist / 90.0));
                self.add_feed(id, killer, boxed, now);
                let by_player = killer != id && (killer as usize) < MAX_PLAYERS;
                if by_player && killer == me && me != NO_ID {
                    self.on_my_kill(id, x, y, boxed, now);
                }
                if by_player && self.phase == phase::PLAYING {
                    self.first_blood = true;
                }
            }
            msg::WALLS_GONE => {
                let c = self.cycle(r.u8());
                c.walls = false;
                c.gone_ms = Some(now);
            }
            msg::FULL => {
                let tick = r.u32() as f64;
                self.phase = r.u8();
                self.phase_end = r.u32() as f64;
                self.round = r.u8();
                self.rounds = r.u8();
                self.winner = NO_ID;
                self.killed_by = NO_ID;
                self.killed_boxed = false;
                self.first_blood = false;
                if self.round == 1 {
                    self.streak = 0;
                    self.nemesis = NO_ID;
                }
                self.cycles.clear();
                self.pending.clear();
                self.zone = 0.0;
                self.cam_snap = true;
                let n = r.u8();
                for _ in 0..n {
                    let id = self.read_cycle(&mut r, tick);
                    let np = r.u16();
                    let pts: Vec<(f32, f32)> = (0..np).map(|_| (r.f32(), r.f32())).collect();
                    self.cycle(id).trail = pts;
                }
                if !self.clock_ok {
                    self.offset = tick - now * TICK_HZ / 1000.0;
                }
                if self.braking {
                    send(&W::new(msg::C_BRAKE).u8(1).done());
                }
            }
            msg::ROSTER => {
                self.players.clear();
                let n = r.u8();
                for _ in 0..n {
                    let id = r.u8();
                    let col = [r.u8(), r.u8(), r.u8()];
                    let bot = r.u8() != 0;
                    let score = r.i16();
                    let kills = r.u16();
                    let deaths = r.u16();
                    let len = r.u8() as usize;
                    let name = String::from_utf8_lossy(r.bytes(len)).into_owned();
                    // Brighten dark picks so every wall glows.
                    let mut color = col.map(|v| v as f32 / 255.0);
                    let m = color[0].max(color[1]).max(color[2]).max(0.01);
                    if m < 0.7 {
                        color = color.map(|v| (v / m * 0.7).max(0.05));
                    }
                    self.players.push(Player { id, color, bot, score, kills, deaths, name });
                }
            }
            msg::PHASE => {
                self.phase = r.u8();
                self.phase_end = r.u32() as f64;
                self.winner = r.u8();
                self.round = r.u8();
                self.rounds = r.u8();
                if self.phase == phase::ROUND_OVER || self.phase == phase::MATCH_OVER {
                    if self.my_id != NO_ID && self.cycles.iter().any(|c| c.id == self.my_id) {
                        play(snd::RESULT, (self.winner == self.my_id) as u32 as f32, 0.0);
                        if self.winner == self.my_id {
                            self.callout("voice", "LAST PROGRAM STANDING");
                        }
                    }
                }
            }
            msg::PONG => {
                let (sent, server) = (r.f64(), r.f64());
                let rtt = (now - sent).max(0.0);
                let sample = server + rtt * 0.5 * TICK_HZ / 1000.0 - now * TICK_HZ / 1000.0;
                self.rtt = if self.pings == 0 { rtt } else { self.rtt + (rtt - self.rtt) * 0.2 };
                self.pings += 1;
                // Trust low-latency samples most: queueing delay only ever adds error.
                self.best_rtt += 0.5;
                if !self.clock_ok || (sample - self.offset).abs() > 30.0 {
                    self.offset = sample;
                    self.best_rtt = rtt;
                    self.clock_ok = true;
                } else if rtt <= self.best_rtt * 1.15 + 1.0 {
                    self.best_rtt = self.best_rtt.min(rtt);
                    self.offset += (sample - self.offset) * 0.3;
                }
            }
            _ => {}
        }
    }

    /// Kill feed line: "hex\tname\tverb\thex\tname" (second name optional).
    fn add_feed(&mut self, id: u8, killer: u8, boxed: bool, now: f64) {
        let victim = (hex(color_of(&self.players, id)), self.name(id));
        let text = if killer == id {
            format!("{}\t{}\thit their own wall\t\t", victim.0, victim.1)
        } else if killer == NO_ID {
            format!("{}\t{}\tcrashed into the rim\t\t", victim.0, victim.1)
        } else if killer == ZONE_ID {
            format!("{}\t{}\twas caught by the zone\t\t", victim.0, victim.1)
        } else {
            let verb = if boxed { "boxed in" } else { "derezzed" };
            format!("{}\t{}\t{verb}\t{}\t{}", hex(color_of(&self.players, killer)), self.name(killer), victim.0, victim.1)
        };
        self.feed.push(FeedLine { text, until: now + 6000.0 });
        if self.feed.len() > 5 {
            self.feed.remove(0);
        }
    }

    /// Everything that makes a kill feel like a kill: marker, floating points,
    /// a burst in our colour, camera punch and the announcer.
    fn on_my_kill(&mut self, victim: u8, x: f32, y: f32, boxed: bool, now: f64) {
        let pts = score::KILL + if boxed { score::BOX } else { 0 };
        let text = format!("+{pts}  {}", self.name(victim).to_uppercase());
        unsafe { hud(slot::HIT, text.as_ptr(), text.len()) };
        play(snd::HIT, 0.0, 0.0);
        self.popup(x, y, &format!("+{pts}"));
        let mine = color_of(&self.players, self.my_id);
        self.explode(x, y, mine, 90);
        self.shake = self.shake.max(0.7);
        self.fov_kick = self.fov_kick.max(0.12);

        self.multi = if now - self.last_kill_ms < 4000.0 { self.multi + 1 } else { 1 };
        self.last_kill_ms = now;
        self.streak += 1;
        let mut calls: Vec<(&str, &str)> = Vec::new();
        if boxed {
            calls.push(("kill", "BOXED"));
        }
        if !self.first_blood {
            calls.push(("kill", "FIRST BLOOD"));
        }
        match self.multi {
            2 => calls.push(("spree", "DOUBLE KILL")),
            3 => calls.push(("spree", "TRIPLE KILL")),
            n if n >= 4 => calls.push(("spree", "MULTI KILL")),
            _ => {}
        }
        if victim == self.nemesis {
            calls.push(("kill", "REVENGE"));
            self.nemesis = NO_ID;
        }
        if let Some(&(_, name)) = STREAKS.iter().find(|s| s.0 == self.streak) {
            calls.push(("spree", name));
        }
        if !calls.is_empty() {
            play(snd::STING, calls.len() as f32 + self.multi.min(4) as f32, 0.0);
        }
        for (style, text) in calls {
            self.callout(style, text);
        }
    }

    fn callout(&self, style: &str, text: &str) {
        let s = format!("{style}\t{text}");
        unsafe { hud(slot::CALLOUT, s.as_ptr(), s.len()) };
    }

    /// Floating text at a ground position, projected with last frame's camera.
    fn popup(&self, x: f32, y: f32, text: &str) {
        let m = &self.uniforms;
        let p = [x, 1.5, -y, 1.0];
        let clip: Vec<f32> = (0..4).map(|r| (0..4).map(|k| m[k * 4 + r] * p[k]).sum()).collect();
        // Off screen: pin it to the nearest edge (behind us: bottom centre).
        let (fx, fy) = if clip[3] > 0.1 {
            (0.5 + 0.5 * clip[0] / clip[3], 0.5 - 0.5 * clip[1] / clip[3])
        } else {
            (0.5, 0.8)
        };
        let (fx, fy) = (fx.clamp(0.06, 0.94), fy.clamp(0.12, 0.88));
        let s = format!("{fx:.4} {fy:.4}\t{text}");
        unsafe { hud(slot::POP, s.as_ptr(), s.len()) };
    }

    fn explode(&mut self, x: f32, y: f32, col: [f32; 3], n: u32) {
        for _ in 0..n {
            let a = self.rand() * TAU;
            let up = self.rand();
            let sp = 4.0 + self.rand() * 26.0;
            let max = 0.6 + self.rand() * 1.4;
            let horiz = (1.0 - up * up).sqrt();
            self.particles.push(Particle {
                p: [x, 0.7, -y],
                v: [a.cos() * horiz * sp, up * sp * 0.9 + 3.0, a.sin() * horiz * sp],
                life: max,
                max,
                col,
            });
        }
        self.rings.push(Ring { x, y, age: 0.0, col });
    }

    fn turn_input(&mut self, left: bool, now: f64) {
        if !self.connected || !matches!(self.phase, phase::COUNTDOWN | phase::PLAYING | phase::ROUND_OVER | phase::MATCH_OVER) {
            return;
        }
        let me = self.my_id;
        let Some(c) = self.cycles.iter().find(|c| c.id == me && c.alive) else { return };
        if self.pending.len() >= 8 {
            return;
        }
        let cur = self.pending.last().map_or(c.dir, |p| p.dir);
        // Close call: a wall was about to arrive (~120 ms out) and the new way is open.
        let skip = |s: &Seg| s.owner == me && s.current;
        let soon = c.speed * 0.12;
        let ahead = raycast(c.hx, c.hy, c.hdir, soon, &self.segs, skip);
        let close = self.phase == phase::PLAYING
            && c.hdir == cur
            && ahead > 0.05
            && ahead < soon
            && raycast(c.hx, c.hy, turn(cur, left), 3.0, &self.segs, skip) >= 3.0
            && now >= self.close_until;
        let t = self.present(now);
        self.seq = self.seq.wrapping_add(1);
        if self.phase != phase::COUNTDOWN {
            self.pending.push(Pending { seq: self.seq, t, dir: turn(cur, left) });
        }
        send(&W::new(msg::C_TURN).u8(self.seq).u8(left as u8).f64(t).done());
        play(snd::TURN, 0.0, 0.0);
        if close {
            self.close_until = now + 1500.0;
            self.callout("close", "CLOSE CALL");
            play(snd::CLOSE, 0.0, 0.0);
            self.fov_kick = self.fov_kick.max(0.06);
        }
    }

    fn brake_input(&mut self, on: bool) {
        if self.braking != on {
            self.braking = on;
            if self.connected {
                send(&W::new(msg::C_BRAKE).u8(on as u8).done());
            }
        }
    }

    /// Positions every cycle head at present time `p`, honouring local unacked turns.
    fn update_heads(&mut self, p: f64, dt: f32) {
        let me = self.my_id;
        self.pending.retain(|q| q.t > p - 60.0);
        self.my_extra.clear();
        for c in self.cycles.iter_mut() {
            if !c.alive {
                c.hx = c.x;
                c.hy = c.y;
                c.hdir = c.dir;
                continue;
            }
            let v = c.speed / TICK_HZ as f32;
            let (mut x, mut y, mut d, mut t) = (c.x, c.y, c.dir, c.t);
            if c.id == me {
                for q in &self.pending {
                    let dt = (q.t - t) as f32;
                    x += DIRS[d as usize].0 * v * dt;
                    y += DIRS[d as usize].1 * v * dt;
                    self.my_extra.push((x, y));
                    d = q.dir;
                    t = q.t;
                }
            }
            // Don't run ahead more than half a second if updates stall.
            let dt = (p - t).clamp(-30.0, 30.0) as f32;
            c.hx = x + DIRS[d as usize].0 * v * dt;
            c.hy = y + DIRS[d as usize].1 * v * dt;
            c.hdir = d;
        }

        // Stop extrapolated heads at known walls so nobody visibly drives through one.
        self.segs.clear();
        self.segs.extend_from_slice(&rim());
        if self.zone > 0.0 {
            self.segs.extend_from_slice(&square(self.zone, ZONE_ID));
        }
        for c in &self.cycles {
            if !c.walls {
                continue;
            }
            let extra: &[(f32, f32)] = if c.id == me { &self.my_extra } else { &[] };
            let mut prev: Option<(f32, f32)> = None;
            for &pt in c.trail.iter().chain(extra) {
                if let Some(a) = prev {
                    self.segs.push(Seg { ax: a.0, ay: a.1, bx: pt.0, by: pt.1, owner: c.id, current: false });
                }
                prev = Some(pt);
            }
        }
        for c in self.cycles.iter_mut() {
            let goal = c.hdir as f32 * FRAC_PI_2;
            if !c.vis_init {
                c.vis_yaw = goal;
                c.vis_init = true;
            }
            c.vis_yaw += wrap(goal - c.vis_yaw) * (1.0 - (-22.0 * dt).exp());
            if !c.alive {
                continue;
            }
            let extra = if c.id == me { self.my_extra.last().copied() } else { None };
            let Some((sx, sy)) = extra.or(c.trail.last().copied()) else { continue };
            let len = (c.hx - sx).abs() + (c.hy - sy).abs();
            let (dx, dy) = DIRS[c.hdir as usize];
            // Only clamp when the head lies ahead of its segment start.
            if len > 1e-4 && ((c.hx - sx) * dx + (c.hy - sy) * dy) > 0.0 {
                let free = raycast(sx, sy, c.hdir, len, &self.segs, |_| false);
                if free < len {
                    let free = (free - 0.02).max(0.0);
                    c.hx = sx + dx * free;
                    c.hy = sy + dy * free;
                }
            }
        }
        // Each live wall's growing end, for grinding and close-call checks.
        for c in &self.cycles {
            let extra = if c.id == me { self.my_extra.last().copied() } else { None };
            if let (true, true, Some((sx, sy))) = (c.alive, c.walls, extra.or(c.trail.last().copied())) {
                self.segs.push(Seg { ax: sx, ay: sy, bx: c.hx, by: c.hy, owner: c.id, current: true });
            }
        }
    }

    /// Nearest wall running alongside cycle `i` within GRIND_RANGE, like the
    /// server's speed boost: (distance, +1 left / -1 right, owner).
    fn grind_probe(&self, i: usize) -> Option<(f32, f32, u8)> {
        let c = &self.cycles[i];
        let (lx, ly) = DIRS[turn(c.hdir, true) as usize];
        let along_x = DIRS[c.hdir as usize].0 != 0.0;
        let mut best: Option<(f32, f32, u8)> = None;
        for s in &self.segs {
            if s.owner == c.id && s.current {
                continue;
            }
            let off = if along_x {
                if (s.ay - s.by).abs() > 1e-6 || c.hx < s.ax.min(s.bx) || c.hx > s.ax.max(s.bx) { continue }
                (s.ay - c.hy) * ly
            } else {
                if (s.ax - s.bx).abs() > 1e-6 || c.hy < s.ay.min(s.by) || c.hy > s.ay.max(s.by) { continue }
                (s.ax - c.hx) * lx
            };
            if off.abs() < best.map_or(GRIND_RANGE, |b| b.0) {
                best = Some((off.abs(), off.signum(), s.owner));
            }
        }
        best
    }

    /// Sparks off the wall you're riding; the closer, the more.
    fn update_grind(&mut self, dt: f32) {
        let moving = self.phase == phase::PLAYING || self.phase == phase::ROUND_OVER;
        let i = self.cycles.iter().position(|c| c.id == self.follow && c.alive);
        let probe = i.filter(|_| moving).and_then(|i| self.grind_probe(i).map(|p| (i, p)));
        let target = probe.map_or(0.0, |(_, (d, _, _))| 1.0 - d / GRIND_RANGE);
        self.grind += (target - self.grind) * (1.0 - (-14.0 * dt).exp());
        let Some((i, (d, side, owner))) = probe else { return };
        if target < 0.25 {
            return;
        }
        let c = &self.cycles[i];
        let (fx, fy) = DIRS[c.hdir as usize];
        let (lx, ly) = DIRS[turn(c.hdir, true) as usize];
        let (px, py) = (c.hx + lx * side * d - fx * 0.4, c.hy + ly * side * d - fy * 0.4);
        let speed = c.speed;
        let wall = if (owner as usize) < MAX_PLAYERS { color_of(&self.players, owner) } else { [0.55, 0.85, 1.0] };
        let col = wall.map(|v| v * 0.5 + 0.5);
        self.spark_acc += target * target * 320.0 * dt;
        while self.spark_acc >= 1.0 {
            self.spark_acc -= 1.0;
            let back = speed * (0.12 + self.rand() * 0.3);
            let away = 2.0 + self.rand() * 5.0;
            let max = 0.18 + self.rand() * 0.3;
            let h = 0.1 + self.rand() * 0.5;
            let (vx, vy) = (-fx * back - lx * side * away, -fy * back - ly * side * away);
            let up = 2.0 + self.rand() * 6.0;
            self.particles.push(Particle {
                p: [px, h, -py],
                v: [vx, up, -vy],
                life: max,
                max,
                col,
            });
        }
    }

    fn frame(&mut self, now: f64, w: f32, h: f32) -> usize {
        let dt = if self.last_ms == 0.0 { 0.016 } else { ((now - self.last_ms) / 1000.0).clamp(0.0, 0.1) as f32 };
        self.last_ms = now;
        self.now = now;
        self.fps_frames += 1;
        if now - self.fps_t0 >= 500.0 {
            self.fps = (self.fps_frames as f64 * 1000.0 / (now - self.fps_t0)) as f32;
            self.fps_frames = 0;
            self.fps_t0 = now;
        }
        if self.connected && now >= self.next_ping {
            send(&W::new(msg::C_PING).f64(now).done());
            self.next_ping = now + if self.pings < 10 { 150.0 } else { 1000.0 };
        }

        let p = self.present(now);
        self.update_heads(p, dt);
        self.update_grind(dt);
        self.step_effects(dt);
        self.verts.clear();
        let cam = self.camera(dt, w / h.max(1.0));
        self.cam_eye = cam.eye;
        render::build(self, &cam, now, w, h);
        if now >= self.next_hud {
            self.next_hud = now + 50.0;
            self.update_hud(p);
        }
        self.verts.len()
    }

    fn step_effects(&mut self, dt: f32) {
        for q in self.particles.iter_mut() {
            q.v[1] -= 22.0 * dt;
            for k in 0..3 {
                q.p[k] += q.v[k] * dt;
            }
            if q.p[1] < 0.05 {
                q.p[1] = 0.05;
                q.v[1] *= -0.35;
                q.v[0] *= 0.7;
                q.v[2] *= 0.7;
            }
            q.life -= dt;
        }
        self.particles.retain(|q| q.life > 0.0);
        for r in self.rings.iter_mut() {
            r.age += dt;
        }
        self.rings.retain(|r| r.age < 0.9);
        self.shake *= (-6.0 * dt).exp();
        self.fov_kick *= (-7.0 * dt).exp();
    }

    fn camera(&mut self, dt: f32, aspect: f32) -> render::Camera {
        // Follow ourselves, else keep watching whoever we were following, else the first survivor.
        let me = self.my_id;
        let follow_ok = |id: u8| {
            self.cycles.iter().find(|c| c.id == id).is_some_and(|c| c.alive || self.now - c.death_ms < 1500.0)
        };
        let alive = |id: u8| self.cycles.iter().any(|c| c.id == id && c.alive);
        if follow_ok(me) {
            self.follow = me;
        } else if (self.follow == me || !follow_ok(self.follow)) && self.killed_by != me && alive(self.killed_by) {
            // Killer cam: see what the one who got you does next.
            self.follow = self.killed_by;
        } else if !follow_ok(self.follow) {
            self.follow = self.cycles.iter().find(|c| c.alive).map_or(NO_ID, |c| c.id);
        }
        let target = self.cycles.iter().find(|c| c.id == self.follow);
        let a = ARENA;
        let mut hide = NO_ID;
        let (eye, look, fov, near) = match (self.cam_mode, target) {
            (0 | 1, Some(c)) => {
                let goal = c.hdir as f32 * FRAC_PI_2;
                if std::mem::take(&mut self.cam_snap) {
                    self.cam_yaw = goal;
                }
                let rate = if self.cam_mode == 0 { 7.0 } else { 16.0 };
                self.cam_yaw += wrap(goal - self.cam_yaw) * (1.0 - (-rate * dt).exp());
                let (fx, fy) = (self.cam_yaw.cos(), self.cam_yaw.sin());
                let boost = ((c.speed - BASE_SPEED) / (MAX_SPEED - BASE_SPEED)).clamp(0.0, 1.0);
                if self.cam_mode == 0 {
                    let back = 12.5 + boost * 4.0;
                    let up = 6.2 + boost * 1.2;
                    ([c.hx - fx * back, up, -(c.hy - fy * back)], [c.hx + fx * 14.0, 0.0, -(c.hy + fy * 14.0)], 1.1 + boost * 0.35, 0.5)
                } else {
                    if c.alive {
                        hide = c.id;
                    }
                    let (ex, ey) = (c.hx - fx * 0.9, c.hy - fy * 0.9);
                    ([ex, 1.15, -ey], [ex + fx * 30.0, 0.6, -(ey + fy * 30.0)], 1.3 + boost * 0.35, 0.1)
                }
            }
            (2, Some(c)) => ([c.hx, 120.0, -c.hy + 55.0], [c.hx, 0.0, -c.hy], 1.0, 0.5),
            _ => ([a * 0.5, a * 0.82, -a * 0.5 + a * 0.62], [a * 0.5, 0.0, -a * 0.5 + a * 0.04], 1.0, 0.5),
        };
        // Shake: impacts plus a fast rumble while grinding at speed.
        let rumble = if self.cam_mode < 2 { self.grind * 0.05 } else { 0.0 };
        let amp = (self.shake + rumble) * if self.cam_mode == 1 { 0.25 } else { 1.0 };
        let mut eye = eye;
        let mut look = look;
        if amp > 0.002 {
            for k in 0..3 {
                let j = (self.rand() - 0.5) * amp;
                eye[k] += j;
                look[k] += j * 0.5;
            }
            eye[1] = eye[1].max(0.3);
        }
        let fov = fov * (1.0 - self.fov_kick);
        let view = Mat4::look_at(eye, look);
        let proj = Mat4::perspective(fov, aspect, near, 3000.0);
        render::Camera { vp: proj.mul(&view), eye, right: view.row(0), up: view.row(1), hide }
    }

    fn update_hud(&mut self, p: f64) {
        // Scoreboard: "rrggbb\tname\tscore\tflags\tkills\tdeaths" lines, sorted by score.
        let mut order: Vec<usize> = (0..self.players.len()).collect();
        order.sort_by_key(|&i| (-self.players[i].score, self.players[i].id));
        let mut board = String::new();
        for i in order {
            let pl = &self.players[i];
            let alive = self.cycles.iter().any(|c| c.id == pl.id && c.alive);
            let flags = format!("{}{}{}", if alive { "a" } else { "" }, if pl.id == self.my_id { "m" } else { "" }, if pl.bot { "b" } else { "" });
            board.push_str(&format!("{}\t{}\t{}\t{flags}\t{}\t{}\n", hex(pl.color), pl.name, pl.score, pl.kills, pl.deaths));
        }
        self.set_hud(slot::BOARD, board);

        let me_alive = self.cycles.iter().any(|c| c.id == self.my_id && c.alive);
        let me_in_round = self.cycles.iter().any(|c| c.id == self.my_id);
        let dead_for = self.cycles.iter().find(|c| c.id == self.my_id && !c.alive).map_or(0.0, |c| self.now - c.death_ms);
        let top_player = self.players.iter().max_by_key(|p| (p.score, -(p.id as i16))).map(|p| p.id);
        let count = if self.phase == phase::COUNTDOWN { ((self.phase_end - p) / TICK_HZ).ceil().max(1.0) as i32 } else { 0 };
        if count != self.last_count {
            if count > 0 {
                play(snd::BEEP, count as f32, 0.0);
            } else if self.phase == phase::PLAYING {
                play(snd::GO, 0.0, 0.0);
            }
            self.last_count = count;
        }
        let center = if !self.connected {
            "CONNECTING…".to_string()
        } else {
            match self.phase {
                phase::COUNTDOWN => format!("{count}"),
                phase::PLAYING if me_in_round && !me_alive && dead_for < 3000.0 => match self.killed_by {
                    k if k == self.my_id || k == NO_ID => format!("DEREZZED  {}", score::SUICIDE),
                    ZONE_ID => format!("CAUGHT BY THE ZONE  {}", score::DEATH),
                    k if self.killed_boxed => format!("BOXED IN BY {}  {}", self.name(k).to_uppercase(), score::DEATH),
                    k => format!("DEREZZED BY {}  {}", self.name(k).to_uppercase(), score::DEATH),
                },
                phase::PLAYING if p - (self.phase_end - 180.0 * TICK_HZ) < 40.0 => "GO".to_string(),
                phase::ROUND_OVER if self.winner == self.my_id && self.my_id != NO_ID => "YOU WIN THE ROUND".to_string(),
                phase::ROUND_OVER if self.winner != NO_ID => format!("{} WINS THE ROUND", self.name(self.winner).to_uppercase()),
                phase::ROUND_OVER => "ROUND OVER".to_string(),
                phase::MATCH_OVER => match top_player {
                    Some(id) if id == self.my_id => "YOU WIN THE MATCH".to_string(),
                    Some(id) => format!("{} WINS THE MATCH", self.name(id).to_uppercase()),
                    None => "MATCH OVER".to_string(),
                },
                _ => String::new(),
            }
        };
        self.set_hud(slot::CENTER, center);

        let round = match self.phase {
            phase::WAITING => String::new(),
            phase::PLAYING if self.zone > 0.0 => format!("ROUND {} / {} · SUDDEN DEATH", self.round, self.rounds),
            _ => format!("ROUND {} / {}", self.round, self.rounds),
        };
        self.set_hud(slot::ROUND, round);

        let shown = self.cycles.iter().find(|c| c.id == self.follow && c.alive).map(|c| (c.speed, c.brake));
        let rtt = if self.rtt < 10.0 { format!("{:.1}", self.rtt) } else { format!("{:.0}", self.rtt) };
        let stats = format!("{:.0} fps · {rtt} ms · cam {}", self.fps, CAM_NAMES[self.cam_mode as usize]);
        self.set_hud(slot::STATS, stats);
        let meters = shown.map_or(String::new(), |(speed, brake)| format!("{:.0} {:.3} {:.3}", speed, speed / MAX_SPEED, brake));
        self.set_hud(slot::METERS, meters);
        let mine = self.cycles.iter().find(|c| c.id == self.my_id && c.alive);
        match mine {
            Some(c) => {
                let frac = ((c.speed - BRAKE_SPEED) / (MAX_SPEED - BRAKE_SPEED)).clamp(0.0, 1.0);
                play(snd::ENGINE, frac, c.braking as u32 as f32);
                play(snd::GRIND, self.grind, frac);
            }
            None => {
                play(snd::ENGINE, -1.0, 0.0);
                play(snd::GRIND, 0.0, 0.0);
            }
        }
        // Screen edges glow in your colour as you pick up speed.
        let edge = mine.map_or(String::new(), |c| {
            let boost = ((c.speed - BASE_SPEED) / (MAX_SPEED - BASE_SPEED)).clamp(0.0, 1.0);
            let a = (boost * 0.6 + self.grind * 0.25).min(0.8);
            format!("{} {:.2}", hex(color_of(&self.players, c.id)), a)
        });
        self.set_hud(slot::EDGE, edge);

        let now = self.now;
        self.feed.retain(|f| f.until > now);
        let feed: String = self.feed.iter().map(|f| format!("{}\n", f.text)).collect();
        self.set_hud(slot::FEED, feed);

        let watching = if self.follow != NO_ID && self.follow != self.my_id { self.name(self.follow) } else { String::new() };
        let hint = if self.my_id == NO_ID {
            "Spectating — press Enter to join".to_string()
        } else if !me_in_round {
            "You'll spawn next round".to_string()
        } else if !me_alive && !watching.is_empty() && self.phase == phase::PLAYING {
            let left = self.cycles.iter().filter(|c| c.alive).count();
            let who = if self.follow == self.killed_by { " (your killer)" } else { "" };
            format!("Watching {watching}{who} · {left} left · you're back next round")
        } else {
            String::new()
        };
        self.set_hud(slot::HINT, hint);
    }

    fn set_hud(&mut self, slot: u32, text: String) {
        let cache = &mut self.hud_cache[slot as usize];
        if *cache != text {
            unsafe { hud(slot, text.as_ptr(), text.len()) };
            *cache = text;
        }
    }
}

// ------------------------------------------------------------------ exports

#[no_mangle]
pub extern "C" fn init() {
    std::panic::set_hook(Box::new(|info| {
        let s = info.to_string();
        unsafe { console_log(s.as_ptr(), s.len()) };
    }));
    st();
}

/// Returns a buffer of `len` bytes for JS to fill before `on_message` / `join`.
#[no_mangle]
pub extern "C" fn inbox(len: usize) -> *mut u8 {
    let s = st();
    s.inbox.resize(len, 0);
    s.inbox.as_mut_ptr()
}

#[no_mangle]
pub extern "C" fn on_message(len: usize, now: f64) {
    let s = st();
    let data = std::mem::take(&mut s.inbox);
    s.on_message(&data[..len.min(data.len())], now);
    s.inbox = data;
}

#[no_mangle]
pub extern "C" fn on_open(now: f64) {
    let s = st();
    // Fresh state for a new connection or lobby; only view settings carry over.
    let (mode, minimap) = (s.cam_mode, s.minimap);
    *s = State::new();
    s.cam_mode = mode;
    s.minimap = minimap;
    s.connected = true;
    s.next_ping = now;
    // The DOM still shows whatever was set before the reset: resend every slot.
    s.hud_cache = std::array::from_fn(|_| "\0".to_string());
}

#[no_mangle]
pub extern "C" fn on_close() {
    let s = st();
    s.connected = false;
    s.clock_ok = false;
    s.pings = 0;
}

/// 1/2 turn left/right, 3 next camera, 4/5 brake down/up, 6 toggle minimap,
/// 10..=13 pick camera directly.
#[no_mangle]
pub extern "C" fn on_key(code: u32, now: f64) {
    let s = st();
    match code {
        1 => s.turn_input(true, now),
        2 => s.turn_input(false, now),
        3 => s.cam_mode = (s.cam_mode + 1) % 4,
        4 => s.brake_input(true),
        5 => s.brake_input(false),
        6 => s.minimap = !s.minimap,
        10..=13 => s.cam_mode = (code - 10) as u8,
        _ => {}
    }
}

/// Joins (or renames / recolours) with the UTF-8 name previously written to the inbox.
#[no_mangle]
pub extern "C" fn join(len: usize, r: u32, g: u32, b: u32) {
    let s = st();
    let name = &s.inbox[..len.min(s.inbox.len()).min(48)];
    send(&W::new(msg::C_JOIN).u8(r as u8).u8(g as u8).u8(b as u8).u8(name.len() as u8).bytes(name).done());
}

#[no_mangle]
pub extern "C" fn frame(now: f64, w: f32, h: f32) -> usize {
    st().frame(now, w, h)
}

#[no_mangle]
pub extern "C" fn verts_ptr() -> *const Vert {
    st().verts.as_ptr()
}

#[no_mangle]
pub extern "C" fn uniforms_ptr() -> *const f32 {
    st().uniforms.as_ptr()
}
