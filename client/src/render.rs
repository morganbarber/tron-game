//! Scene geometry. Everything is emitted into `State::verts` as four ranges:
//!   1. opaque   – bike shells, arena border panels (depth write)
//!   2. glow     – light pooled on the floor, blended with MAX so overlapping
//!                 trail/bike glows merge into one shape instead of stacking
//!   3. additive – light walls, neon trim, particles, shockwaves
//!   4. overlay  – minimap, already in clip space
//! Game (x, y) maps to world (x, height, -y).

use crate::{color_of, State};
use common::*;
use std::f32::consts::TAU;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Vert {
    pub p: [f32; 3],
    pub c: [u8; 4],
}

/// Column-major 4x4 matrix as WebGL expects.
#[derive(Clone, Copy)]
pub struct Mat4(pub [f32; 16]);

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt().max(1e-9);
    [a[0] / l, a[1] / l, a[2] / l]
}

impl Mat4 {
    pub fn look_at(eye: [f32; 3], target: [f32; 3]) -> Mat4 {
        let f = norm(sub(target, eye));
        let s = norm(cross(f, [0.0, 1.0, 0.0]));
        let u = cross(s, f);
        Mat4([
            s[0], u[0], -f[0], 0.0,
            s[1], u[1], -f[1], 0.0,
            s[2], u[2], -f[2], 0.0,
            -dot(s, eye), -dot(u, eye), dot(f, eye), 1.0,
        ])
    }

    pub fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fovy * 0.5).tan();
        let nf = 1.0 / (near - far);
        Mat4([
            f / aspect, 0.0, 0.0, 0.0,
            0.0, f, 0.0, 0.0,
            0.0, 0.0, (far + near) * nf, -1.0,
            0.0, 0.0, 2.0 * far * near * nf, 0.0,
        ])
    }

    pub fn mul(&self, b: &Mat4) -> Mat4 {
        let mut o = [0.0; 16];
        for c in 0..4 {
            for r in 0..4 {
                o[c * 4 + r] = (0..4).map(|k| self.0[k * 4 + r] * b.0[c * 4 + k]).sum();
            }
        }
        Mat4(o)
    }

    /// Row `r` of the rotation part (camera right / up axes for a view matrix).
    pub fn row(&self, r: usize) -> [f32; 3] {
        [self.0[r], self.0[4 + r], self.0[8 + r]]
    }
}

pub struct Camera {
    pub vp: Mat4,
    pub eye: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    /// Cycle whose model is hidden (first-person view from inside it).
    pub hide: u8,
}

#[inline]
fn rgba(c: [f32; 3], k: f32) -> [u8; 4] {
    let q = |v: f32| (v * k * 255.0).clamp(0.0, 255.0) as u8;
    [q(c[0]), q(c[1]), q(c[2]), 255]
}

#[inline]
fn mix(c: [f32; 3], d: [f32; 3], t: f32) -> [f32; 3] {
    [c[0] + (d[0] - c[0]) * t, c[1] + (d[1] - c[1]) * t, c[2] + (d[2] - c[2]) * t]
}

#[inline]
fn quad(v: &mut Vec<Vert>, p: [[f32; 3]; 4], c: [[u8; 4]; 4]) {
    for i in [0, 1, 2, 0, 2, 3] {
        v.push(Vert { p: p[i], c: c[i] });
    }
}

#[inline]
fn tri(v: &mut Vec<Vert>, p: [[f32; 3]; 3], c: [[u8; 4]; 3]) {
    for i in 0..3 {
        v.push(Vert { p: p[i], c: c[i] });
    }
}

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const ZERO: [u8; 4] = [0, 0, 0, 255];
const RIM_COL: [f32; 3] = [0.3, 0.65, 1.0];
const RIM_H: f32 = 7.0;
const ZONE_COL: [f32; 3] = [1.0, 0.12, 0.08];
const WALL_T: f32 = 0.22; // light wall thickness, so it never vanishes edge-on

#[inline]
fn w3(x: f32, h: f32, y: f32) -> [f32; 3] {
    [x, h, -y]
}

// ---------------------------------------------------------------- light walls

/// A light wall as a thin box: gradient sides, a hot top face, optional start cap.
fn light_wall(v: &mut Vec<Vert>, a: (f32, f32), b: (f32, f32), col: [f32; 3], m: f32, cap: bool) {
    let len = ((b.0 - a.0).abs() + (b.1 - a.1).abs()).max(1e-6);
    let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let (px, py) = (-uy * WALL_T * 0.5, ux * WALL_T * 0.5);
    let h = WALL_HEIGHT;
    let hot = mix(col, WHITE, 0.55);
    let base = rgba(hot, 0.75 * m);
    let low = rgba(col, 0.42 * m);
    let mid = rgba(col, 0.2 * m);
    let top = rgba(col, 0.32 * m);
    let core = rgba(hot, 0.9 * m);
    let bands = [(0.0, base), (0.12, low), (h * 0.55, mid), (h - 0.1, top)];
    for side in [1.0f32, -1.0] {
        let (ox, oy) = (px * side, py * side);
        for k in 0..bands.len() - 1 {
            let (h0, c0) = bands[k];
            let (h1, c1) = bands[k + 1];
            quad(
                v,
                [w3(a.0 + ox, h0, a.1 + oy), w3(b.0 + ox, h0, b.1 + oy), w3(b.0 + ox, h1, b.1 + oy), w3(a.0 + ox, h1, a.1 + oy)],
                [c0, c0, c1, c1],
            );
        }
        // Bright lip just under the top.
        quad(
            v,
            [w3(a.0 + ox, h - 0.1, a.1 + oy), w3(b.0 + ox, h - 0.1, b.1 + oy), w3(b.0 + ox, h, b.1 + oy), w3(a.0 + ox, h, a.1 + oy)],
            [core; 4],
        );
    }
    quad(v, [w3(a.0 + px, h, a.1 + py), w3(b.0 + px, h, b.1 + py), w3(b.0 - px, h, b.1 - py), w3(a.0 - px, h, a.1 - py)], [core; 4]);
    if cap {
        quad(v, [w3(a.0 + px, 0.0, a.1 + py), w3(a.0 - px, 0.0, a.1 - py), w3(a.0 - px, h, a.1 - py), w3(a.0 + px, h, a.1 + py)], [low, low, core, core]);
    }
}

/// Soft light on the floor along a segment; three bands approximate a falloff.
fn floor_strip(v: &mut Vec<Vert>, a: (f32, f32), b: (f32, f32), col: [f32; 3], k: f32, width: f32) {
    let len = ((b.0 - a.0).abs() + (b.1 - a.1).abs()).max(1e-6);
    let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let (nx, ny) = (-uy, ux);
    let y = 0.01;
    let rows = [(0.0, rgba(col, k)), (0.35, rgba(col, k * 0.45)), (1.0, ZERO)];
    for side in [1.0f32, -1.0] {
        for r in 0..2 {
            let (d0, c0) = rows[r];
            let (d1, c1) = rows[r + 1];
            let (o0x, o0y) = (nx * d0 * width * side, ny * d0 * width * side);
            let (o1x, o1y) = (nx * d1 * width * side, ny * d1 * width * side);
            quad(
                v,
                [w3(a.0 + o0x, y, a.1 + o0y), w3(b.0 + o0x, y, b.1 + o0y), w3(b.0 + o1x, y, b.1 + o1y), w3(a.0 + o1x, y, a.1 + o1y)],
                [c0, c0, c1, c1],
            );
        }
    }
}

/// Radial floor light.
fn floor_disc(v: &mut Vec<Vert>, x: f32, y: f32, r: f32, col: [f32; 3], k: f32) {
    let n = 20;
    let c0 = rgba(col, k);
    let c1 = rgba(col, k * 0.45);
    let h = 0.01;
    for i in 0..n {
        let a0 = i as f32 / n as f32 * TAU;
        let a1 = (i + 1) as f32 / n as f32 * TAU;
        let p = |a: f32, rr: f32| w3(x + a.cos() * rr, h, y + a.sin() * rr);
        tri(v, [w3(x, h, y), p(a0, r * 0.35), p(a1, r * 0.35)], [c0, c1, c1]);
        quad(v, [p(a0, r * 0.35), p(a1, r * 0.35), p(a1, r), p(a0, r)], [c1, c1, ZERO, ZERO]);
    }
}

// ---------------------------------------------------------------- light cycle

/// Bike frame: forward and right unit vectors in game space plus the head position.
struct Frame {
    x: f32,
    y: f32,
    fx: f32,
    fy: f32,
}

/// Model units to world units.
const BIKE_SCALE: f32 = 1.4;
/// How far behind the head the light wall emerges (the bike's tail).
const BIKE_TAIL: f32 = 2.6 * BIKE_SCALE;

impl Frame {
    fn of(c: &crate::Cyc) -> Frame {
        Frame { x: c.hx, y: c.hy, fx: c.vis_yaw.cos(), fy: c.vis_yaw.sin() }
    }

    /// Local (forward, right, up) to world.
    #[inline]
    fn p(&self, f: f32, s: f32, u: f32) -> [f32; 3] {
        let (f, s, u) = (f * BIKE_SCALE, s * BIKE_SCALE, u * BIKE_SCALE);
        let (rx, ry) = (self.fy, -self.fx);
        w3(self.x + self.fx * f + rx * s, u, self.y + self.fy * f + ry * s)
    }
}

/// Side profile of the shell (forward, up), from the nose clockwise over the top.
const PROFILE: [(f32, f32); 11] = [
    (0.05, 0.42),
    (-0.25, 0.68),
    (-0.75, 0.86),
    (-1.15, 1.02),
    (-1.6, 1.08),
    (-2.05, 0.98),
    (-2.55, 0.78),
    (-2.8, 0.5),
    (-2.6, 0.3),
    (-1.3, 0.24),
    (-0.15, 0.28),
];
const PROFILE_C: (f32, f32) = (-1.35, 0.65);
const BODY_W: f32 = 0.17;
const WHEELS: [(f32, f32); 2] = [(-2.15, 0.52), (-0.55, 0.52)];
const WHEEL_R: f32 = 0.52;
const WHEEL_W: f32 = 0.26;

fn bike_shell(v: &mut Vec<Vert>, fr: &Frame, col: [f32; 3]) {
    let shell = [col[0] * 0.3 + 0.07, col[1] * 0.3 + 0.075, col[2] * 0.3 + 0.09];
    let tyre = [0.02, 0.022, 0.03];
    let n = PROFILE.len();
    // Flanks.
    for s in [BODY_W, -BODY_W] {
        let k = rgba(shell, if s > 0.0 { 0.8 } else { 0.9 });
        for i in 0..n {
            let (a, b) = (PROFILE[i], PROFILE[(i + 1) % n]);
            tri(v, [fr.p(PROFILE_C.0, s, PROFILE_C.1), fr.p(a.0, s, a.1), fr.p(b.0, s, b.1)], [k; 3]);
        }
    }
    // Perimeter, lit from above-front.
    for i in 0..n {
        let (a, b) = (PROFILE[i], PROFILE[(i + 1) % n]);
        let (df, du) = (b.0 - a.0, b.1 - a.1);
        let l = (df * df + du * du).sqrt().max(1e-6);
        let (mut nf, mut nu) = (du / l, -df / l);
        let (mf, mu) = ((a.0 + b.0) * 0.5 - PROFILE_C.0, (a.1 + b.1) * 0.5 - PROFILE_C.1);
        if nf * mf + nu * mu < 0.0 {
            nf = -nf;
            nu = -nu;
        }
        let light = 0.6 + 0.8 * (nf * 0.45 + nu * 0.9).max(0.0);
        let k = rgba(shell, light);
        quad(v, [fr.p(a.0, -BODY_W, a.1), fr.p(b.0, -BODY_W, b.1), fr.p(b.0, BODY_W, b.1), fr.p(a.0, BODY_W, a.1)], [k; 4]);
    }
    // Wheels: short cylinders.
    let seg = 18;
    for &(wf, wu) in &WHEELS {
        for i in 0..seg {
            let a0 = i as f32 / seg as f32 * TAU;
            let a1 = (i + 1) as f32 / seg as f32 * TAU;
            let (c0, s0, c1, s1) = (a0.cos() * WHEEL_R, a0.sin() * WHEEL_R, a1.cos() * WHEEL_R, a1.sin() * WHEEL_R);
            let lit = rgba(tyre, 0.6 + 0.4 * (s0 + s1).max(0.0));
            quad(
                v,
                [fr.p(wf + c0, -WHEEL_W, wu + s0), fr.p(wf + c1, -WHEEL_W, wu + s1), fr.p(wf + c1, WHEEL_W, wu + s1), fr.p(wf + c0, WHEEL_W, wu + s0)],
                [lit; 4],
            );
            for s in [WHEEL_W, -WHEEL_W] {
                let k = rgba(tyre, 1.0);
                tri(v, [fr.p(wf, s, wu), fr.p(wf + c0, s, wu + s0), fr.p(wf + c1, s, wu + s1)], [k; 3]);
            }
        }
    }
}

fn bike_neon(v: &mut Vec<Vert>, fr: &Frame, col: [f32; 3], brake: bool) {
    let hot = rgba(mix(col, WHITE, 0.45), 1.0);
    let soft = rgba(col, 0.55);
    // Glowing wheel rims and hubs.
    let seg = 24;
    for &(wf, wu) in &WHEELS {
        for s in [WHEEL_W + 0.012, -WHEEL_W - 0.012] {
            for i in 0..seg {
                let a0 = i as f32 / seg as f32 * TAU;
                let a1 = (i + 1) as f32 / seg as f32 * TAU;
                let ring = |a: f32, r: f32| fr.p(wf + a.cos() * r, s, wu + a.sin() * r);
                quad(v, [ring(a0, 0.36), ring(a1, 0.36), ring(a1, 0.48), ring(a0, 0.48)], [soft, soft, hot, hot]);
                quad(v, [ring(a0, 0.07), ring(a1, 0.07), ring(a1, 0.12), ring(a0, 0.12)], [soft; 4]);
            }
        }
    }
    // Neon line tracing the upper shell on both flanks.
    for s in [BODY_W + 0.01, -BODY_W - 0.01] {
        for i in 0..7 {
            let (a, b) = (PROFILE[i], PROFILE[i + 1]);
            let t = 0.05;
            quad(v, [fr.p(a.0, s, a.1 - 0.06 - t), fr.p(b.0, s, b.1 - 0.06 - t), fr.p(b.0, s, b.1 - 0.06), fr.p(a.0, s, a.1 - 0.06)], [hot; 4]);
        }
    }
    // Glowing outline of the tail.
    for s in [BODY_W + 0.01, -BODY_W - 0.01] {
        for i in 5..8 {
            let (a, b) = (PROFILE[i], PROFILE[i + 1]);
            quad(v, [fr.p(a.0, s, a.1 - 0.02), fr.p(b.0, s, b.1 - 0.02), fr.p(b.0, s, b.1 + 0.02), fr.p(a.0, s, a.1 + 0.02)], [hot; 4]);
        }
    }
    let (a, b) = (PROFILE[6], PROFILE[7]);
    quad(v, [fr.p(a.0, -BODY_W, a.1 + 0.01), fr.p(b.0, -BODY_W, b.1 + 0.01), fr.p(b.0, BODY_W, b.1 + 0.01), fr.p(a.0, BODY_W, a.1 + 0.01)], [soft; 4]);
    // Lower accent stripe along the belly.
    for s in [BODY_W + 0.01, -BODY_W - 0.01] {
        for i in 8..10 {
            let (a, b) = (PROFILE[i], PROFILE[i + 1]);
            quad(v, [fr.p(a.0, s, a.1 + 0.04), fr.p(b.0, s, b.1 + 0.04), fr.p(b.0, s, b.1 + 0.09), fr.p(a.0, s, a.1 + 0.09)], [soft; 4]);
        }
    }
    // Tail light, red-hot while braking.
    let tail = if brake { rgba([1.0, 0.25, 0.15], 1.0) } else { soft };
    quad(v, [fr.p(-2.81, -0.13, 0.48), fr.p(-2.81, 0.13, 0.48), fr.p(-2.81, 0.13, 0.62), fr.p(-2.81, -0.13, 0.62)], [tail; 4]);
}

// ---------------------------------------------------------------- arena border

fn rim_panels(v: &mut Vec<Vert>) {
    let a = ARENA;
    let dark = rgba([0.018, 0.026, 0.05], 1.0);
    let darker = rgba([0.008, 0.012, 0.025], 1.0);
    let corners = [(0.0, 0.0), (a, 0.0), (a, a), (0.0, a), (0.0, 0.0)];
    for w in corners.windows(2) {
        let (p, q) = (w[0], w[1]);
        quad(v, [w3(p.0, 0.0, p.1), w3(q.0, 0.0, q.1), w3(q.0, RIM_H, q.1), w3(p.0, RIM_H, p.1)], [darker, darker, dark, dark]);
    }
}

fn rim_neon(v: &mut Vec<Vert>) {
    let a = ARENA;
    let hot = rgba(mix(RIM_COL, WHITE, 0.3), 0.9);
    let line = rgba(RIM_COL, 0.55);
    let faint = rgba(RIM_COL, 0.18);
    let o = 0.04; // pulled inward so it sits in front of the panel
    // (start, end, inward normal)
    let sides = [((0.0, 0.0), (a, 0.0), (0.0, 1.0)), ((a, 0.0), (a, a), (-1.0, 0.0)), ((a, a), (0.0, a), (0.0, -1.0)), ((0.0, a), (0.0, 0.0), (1.0, 0.0))];
    for &(p, q, n) in &sides {
        let pp = (p.0 + n.0 * o, p.1 + n.1 * o);
        let qq = (q.0 + n.0 * o, q.1 + n.1 * o);
        let band = |v: &mut Vec<Vert>, h0: f32, h1: f32, c0: [u8; 4], c1: [u8; 4]| {
            quad(v, [w3(pp.0, h0, pp.1), w3(qq.0, h0, qq.1), w3(qq.0, h1, qq.1), w3(pp.0, h1, pp.1)], [c0, c0, c1, c1]);
        };
        band(v, 0.0, 0.14, hot, line);
        band(v, 0.14, 1.2, faint, ZERO);
        band(v, RIM_H - 0.18, RIM_H, line, hot);
        band(v, RIM_H * 0.5 - 0.04, RIM_H * 0.5 + 0.04, faint, faint);
        // Light posts.
        let steps = (a / 25.0) as i32;
        let (ux, uy) = ((q.0 - p.0) / a, (q.1 - p.1) / a);
        for i in 0..=steps {
            let d = i as f32 * 25.0;
            let (cx, cy) = (pp.0 + ux * d, pp.1 + uy * d);
            let hw = if i % 4 == 0 { 0.3 } else { 0.1 };
            let k = if i % 4 == 0 { line } else { faint };
            quad(v, [w3(cx - ux * hw, 0.0, cy - uy * hw), w3(cx + ux * hw, 0.0, cy + uy * hw), w3(cx + ux * hw, RIM_H, cy + uy * hw), w3(cx - ux * hw, RIM_H, cy - uy * hw)], [k; 4]);
        }
    }
}

// ---------------------------------------------------------------- frame

fn wall_fade(c: &crate::Cyc, now: f64) -> f32 {
    match c.gone_ms {
        Some(g) => (1.0 - (now - g) as f32 / 600.0).max(0.0),
        None if !c.walls => 0.0,
        None if !c.alive => 1.0 - ((now - c.death_ms) as f32 / 2000.0).clamp(0.0, 0.45),
        None => 1.0,
    }
}

/// Trail points of `c` including unacknowledged local turns and the head.
fn for_each_segment(s: &State, c: &crate::Cyc, mut f: impl FnMut((f32, f32), (f32, f32), bool)) {
    let extra: &[(f32, f32)] = if c.id == s.my_id { &s.my_extra } else { &[] };
    let start = extra.last().or(c.trail.last()).copied().unwrap_or((c.hx, c.hy));
    let mut head = (c.hx, c.hy);
    if c.alive {
        // The wall comes out of the bike's tail, not through its nose.
        let len = (head.0 - start.0).abs() + (head.1 - start.1).abs();
        if len > 1e-4 {
            let pull = BIKE_TAIL.min(len) / len;
            head = (head.0 - (head.0 - start.0) * pull, head.1 - (head.1 - start.1) * pull);
        }
    }
    let head = [head];
    let mut prev: Option<(f32, f32)> = None;
    let mut first = true;
    for &p in c.trail.iter().chain(extra).chain(head.iter()) {
        if let Some(q) = prev {
            if (p.0 - q.0).abs() + (p.1 - q.1).abs() > 1e-3 {
                f(q, p, first);
                first = false;
            }
        }
        prev = Some(p);
    }
}

pub fn build(s: &mut State, cam: &Camera, now: f64, width: f32, height: f32) {
    let mut v = std::mem::take(&mut s.verts);

    // 1. Opaque.
    rim_panels(&mut v);
    for c in s.cycles.iter().filter(|c| c.alive && c.id != cam.hide) {
        let fr = Frame::of(c);
        bike_shell(&mut v, &fr, color_of(&s.players, c.id));
    }
    let opaque = v.len();

    // 2. Floor glow (MAX blended).
    {
        let a = ARENA;
        let corners = [(0.0, 0.0), (a, 0.0), (a, a), (0.0, a), (0.0, 0.0)];
        for w in corners.windows(2) {
            floor_strip(&mut v, w[0], w[1], RIM_COL, 0.22, 7.0);
        }
    }
    if s.zone > 0.0 {
        let (lo, hi) = (s.zone, ARENA - s.zone);
        let sq = [(lo, lo), (hi, lo), (hi, hi), (lo, hi), (lo, lo)];
        for w in sq.windows(2) {
            floor_strip(&mut v, w[0], w[1], ZONE_COL, 0.5, 6.0);
        }
    }
    for c in &s.cycles {
        let fade = wall_fade(c, now);
        if fade <= 0.0 || c.trail.is_empty() {
            continue;
        }
        let col = color_of(&s.players, c.id);
        for_each_segment(s, c, |a, b, _| floor_strip(&mut v, a, b, col, 0.42 * fade, 2.6));
        if c.alive {
            let back = 1.35 * BIKE_SCALE;
            let (cx, cy) = (c.hx - c.vis_yaw.cos() * back, c.hy - c.vis_yaw.sin() * back);
            floor_disc(&mut v, cx, cy, 4.2, col, 0.5);
        }
    }
    let glow = v.len();

    // 3. Additive.
    rim_neon(&mut v);
    if s.zone > 0.0 {
        // Pulsing translucent curtain that marks the closing zone.
        let (lo, hi) = (s.zone, ARENA - s.zone);
        let pulse = 0.75 + 0.25 * ((now / 140.0) as f32).sin();
        let bot = rgba(ZONE_COL, 0.7 * pulse);
        let top = rgba(mix(ZONE_COL, WHITE, 0.4), pulse);
        let sq = [(lo, lo), (hi, lo), (hi, hi), (lo, hi), (lo, lo)];
        for w in sq.windows(2) {
            let (p, q) = (w[0], w[1]);
            quad(&mut v, [w3(p.0, 0.0, p.1), w3(q.0, 0.0, q.1), w3(q.0, 5.0, q.1), w3(p.0, 5.0, p.1)], [bot, bot, ZERO, ZERO]);
            quad(&mut v, [w3(p.0, 4.85, p.1), w3(q.0, 4.85, q.1), w3(q.0, 5.0, q.1), w3(p.0, 5.0, p.1)], [top; 4]);
        }
    }
    for c in &s.cycles {
        let fade = wall_fade(c, now);
        if fade > 0.0 && !c.trail.is_empty() {
            let col = color_of(&s.players, c.id);
            for_each_segment(s, c, |a, b, first| light_wall(&mut v, a, b, col, fade, first));
        }
        if c.alive && c.id != cam.hide {
            let fr = Frame::of(c);
            bike_neon(&mut v, &fr, color_of(&s.players, c.id), c.braking);
        }
    }
    for r in &s.rings {
        let t = r.age / 0.9;
        let rad = 2.0 + t * 30.0;
        let k = rgba(r.col, (1.0 - t) * 0.9);
        let wht = rgba(WHITE, (1.0 - t) * 0.6);
        let n = 48;
        for i in 0..n {
            let a0 = i as f32 / n as f32 * TAU;
            let a1 = (i + 1) as f32 / n as f32 * TAU;
            let p = |a: f32, rr: f32| w3(r.x + a.cos() * rr, 0.04, r.y + a.sin() * rr);
            quad(&mut v, [p(a0, rad - 1.2), p(a1, rad - 1.2), p(a1, rad), p(a0, rad)], [ZERO, ZERO, wht, wht]);
            quad(&mut v, [p(a0, rad), p(a1, rad), p(a1, rad + 0.4), p(a0, rad + 0.4)], [k, k, ZERO, ZERO]);
        }
    }
    for q in &s.particles {
        let t = q.life / q.max;
        let k = rgba(mix(q.col, WHITE, 0.4 * t), t * 1.3);
        let sz = 0.06 + 0.12 * t;
        let speed = dot(q.v, q.v).sqrt();
        if speed > 4.0 {
            // Fast particles are motion streaks: thin quads facing the camera.
            let tail = [q.p[0] - q.v[0] * 0.03, q.p[1] - q.v[1] * 0.03, q.p[2] - q.v[2] * 0.03];
            let side = norm(cross(q.v, sub(cam.eye, q.p)));
            let w = sz * 0.45;
            let o = |p: [f32; 3], s: f32| [p[0] + side[0] * s, p[1] + side[1] * s, p[2] + side[2] * s];
            let fade = rgba(q.col, 0.0);
            quad(&mut v, [o(tail, -w), o(tail, w), o(q.p, w), o(q.p, -w)], [fade, fade, k, k]);
            continue;
        }
        let (r, u) = (cam.right, cam.up);
        let p = |a: f32, b: f32| [q.p[0] + (r[0] * a + u[0] * b) * sz, q.p[1] + (r[1] * a + u[1] * b) * sz, q.p[2] + (r[2] * a + u[2] * b) * sz];
        quad(&mut v, [p(-1.0, -1.0), p(1.0, -1.0), p(1.0, 1.0), p(-1.0, 1.0)], [k; 4]);
    }
    let additive = v.len();

    // 4. Minimap in clip space, bottom-right.
    if s.minimap && width > 0.0 && height > 0.0 {
        let side = (height * 0.26).min(width * 0.3);
        let (sx, sy) = (2.0 * side / width, 2.0 * side / height);
        let (mx, my) = (2.0 * 14.0 / width, 2.0 * 14.0 / height);
        let (x0, y0) = (1.0 - mx - sx, -1.0 + my);
        let map = |x: f32, y: f32| [x0 + x / ARENA * sx, y0 + y / ARENA * sy, 0.0];
        let (px, py) = (2.0 / width, 2.0 / height);
        let bg = rgba([0.01, 0.02, 0.04], 1.0);
        quad(&mut v, [map(0.0, 0.0), map(ARENA, 0.0), map(ARENA, ARENA), map(0.0, ARENA)], [bg; 4]);
        let line = |v: &mut Vec<Vert>, a: (f32, f32), b: (f32, f32), k: [u8; 4], t: f32| {
            let (pa, pb) = (map(a.0, a.1), map(b.0, b.1));
            let (lx, ly) = (pa[0].min(pb[0]) - px * t, pa[1].min(pb[1]) - py * t);
            let (hx, hy) = (pa[0].max(pb[0]) + px * t, pa[1].max(pb[1]) + py * t);
            quad(v, [[lx, ly, 0.0], [hx, ly, 0.0], [hx, hy, 0.0], [lx, hy, 0.0]], [k; 4]);
        };
        let border = rgba(RIM_COL, 0.7);
        let a = ARENA;
        for (p, q) in [((0.0, 0.0), (a, 0.0)), ((a, 0.0), (a, a)), ((a, a), (0.0, a)), ((0.0, a), (0.0, 0.0))] {
            line(&mut v, p, q, border, 1.0);
        }
        if s.zone > 0.0 {
            let (lo, hi) = (s.zone, ARENA - s.zone);
            let zk = rgba(ZONE_COL, 0.9);
            for (p, q) in [((lo, lo), (hi, lo)), ((hi, lo), (hi, hi)), ((hi, hi), (lo, hi)), ((lo, hi), (lo, lo))] {
                line(&mut v, p, q, zk, 1.0);
            }
        }
        for c in &s.cycles {
            let fade = wall_fade(c, now);
            if fade <= 0.0 {
                continue;
            }
            let k = rgba(color_of(&s.players, c.id), 0.85 * fade);
            for_each_segment(s, c, |a, b, _| line(&mut v, a, b, k, 0.75));
        }
        for c in s.cycles.iter().filter(|c| c.alive) {
            if c.id == s.my_id {
                line(&mut v, (c.hx, c.hy), (c.hx, c.hy), rgba(WHITE, 1.0), 4.5);
            }
            line(&mut v, (c.hx, c.hy), (c.hx, c.hy), rgba(mix(color_of(&s.players, c.id), WHITE, 0.3), 1.0), 3.0);
        }
    }

    s.verts = v;
    s.uniforms[..16].copy_from_slice(&cam.vp.0);
    s.uniforms[16..19].copy_from_slice(&cam.eye);
    s.uniforms[19] = (now / 1000.0 % 10000.0) as f32;
    s.uniforms[20] = opaque as f32;
    s.uniforms[21] = ARENA;
    s.uniforms[22] = glow as f32;
    s.uniforms[23] = additive as f32;
}
