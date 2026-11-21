//! Between the game's axes (z up) and Steam Audio's (y up), and a world of
//! boxes standing in for the game's geometry in tests.

use crate::materials;
use crate::sa::{self, Hit, Ray, Tracer, V3, miss, v3};

/// Game (x east, y north, z up) to Steam Audio (x right, y up, -z north):
/// a rotation, so distances and handedness stay.
pub fn to_sa(p: [f32; 3]) -> V3 {
    v3(p[0], p[2], -p[1])
}

pub fn to_game(v: V3) -> [f32; 3] {
    [v.x, -v.z, v.y]
}

fn cross(a: V3, b: V3) -> V3 {
    v3(a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x)
}

fn norm(a: V3) -> Option<V3> {
    let l = (a.x * a.x + a.y * a.y + a.z * a.z).sqrt();
    (l > 1e-4).then(|| v3(a.x / l, a.y / l, a.z / l))
}

/// A listener at `ear` (game axes) looking along `forward` (game axes, any length).
pub fn listener(ear: [f32; 3], forward: [f32; 3]) -> sa::Space {
    let mut s = sa::Space::at(to_sa(ear));
    let Some(ahead) = norm(to_sa(forward)) else { return s };
    let world_up = v3(0.0, 1.0, 0.0);
    // Looking straight up or down: any right will do.
    let right = norm(cross(ahead, world_up)).unwrap_or(v3(1.0, 0.0, 0.0));
    s.ahead = ahead;
    s.right = right;
    s.up = cross(right, ahead);
    s
}

/// An axis-aligned box in game axes, of one material.
#[derive(Debug, Clone, Copy)]
pub struct Block {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub material: u8,
}

pub fn block(min: [f32; 3], max: [f32; 3], material: u8) -> Block {
    Block { min, max, material }
}

/// Boxes as geometry: the tests' stand-in for the game world.
#[derive(Debug, Clone, Default)]
pub struct Blocks(pub Vec<Block>);

impl Blocks {
    /// Nearest entry into a box along `o + t d` (game axes), t in [min, max]:
    /// (t, game-axes normal, material).
    pub fn trace(&self, o: [f32; 3], d: [f32; 3], min: f32, max: f32) -> Option<(f32, [f32; 3], u8)> {
        let mut best: Option<(f32, [f32; 3], u8)> = None;
        for b in &self.0 {
            let (mut t0, mut t1, mut axis, mut sign) = (f32::NEG_INFINITY, f32::INFINITY, 0, 0.0);
            let mut inside = true;
            for a in 0..3 {
                if d[a].abs() < 1e-9 {
                    if o[a] < b.min[a] || o[a] > b.max[a] {
                        inside = false;
                        break;
                    }
                    continue;
                }
                let (mut ta, mut tb) = ((b.min[a] - o[a]) / d[a], (b.max[a] - o[a]) / d[a]);
                if ta > tb {
                    std::mem::swap(&mut ta, &mut tb);
                }
                if ta > t0 {
                    // The face we enter faces against the ray.
                    (t0, axis, sign) = (ta, a, if d[a] > 0.0 { -1.0 } else { 1.0 });
                }
                t1 = t1.min(tb);
            }
            // A ray starting inside a box sees nothing of it, as in the game.
            if !inside || t0 > t1 || t0 < min || t0 > max {
                continue;
            }
            if best.is_none_or(|(t, ..)| t0 < t) {
                let mut n = [0.0; 3];
                n[axis] = sign;
                best = Some((t0, n, b.material));
            }
        }
        best
    }
}

impl Tracer for Blocks {
    fn closest(&self, rays: &[Ray], min: &[f32], max: &[f32], hits: &mut [Hit]) {
        for (((r, &lo), &hi), h) in rays.iter().zip(min).zip(max).zip(hits.iter_mut()) {
            *h = match self.trace(to_game(r.origin), to_game(r.direction), lo, hi) {
                Some((t, n, mat)) => Hit {
                    distance: t,
                    triangleIndex: -1,
                    objectIndex: -1,
                    // Only `material` is read for a custom scene: an index points into nothing.
                    materialIndex: -1,
                    normal: to_sa(n),
                    material: materials::ptr(mat),
                },
                None => miss(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_round_trip_and_keep_up() {
        let p = [1.0, 2.0, 3.0];
        assert_eq!(to_game(to_sa(p)), p);
        let up = to_sa([0.0, 0.0, 1.0]);
        assert_eq!((up.x, up.y, up.z), (0.0, 1.0, 0.0));
        let l = listener([0.0; 3], [0.0, 1.0, 0.0]);
        // Facing north, east is on the right.
        let east = to_sa([1.0, 0.0, 0.0]);
        assert!((l.right.x - east.x).abs() < 1e-6 && (l.right.z - east.z).abs() < 1e-6);
    }

    #[test]
    fn boxes_are_hit_on_the_near_face() {
        let w = Blocks(vec![block([2.0, -5.0, 0.0], [3.0, 5.0, 3.0], materials::CONCRETE)]);
        let (t, n, m) = w.trace([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.0, 100.0).unwrap();
        assert!((t - 2.0).abs() < 1e-5 && n == [-1.0, 0.0, 0.0] && m == materials::CONCRETE);
        assert!(w.trace([0.0, 0.0, 1.0], [-1.0, 0.0, 0.0], 0.0, 100.0).is_none());
        assert!(w.trace([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.0, 1.5).is_none());
    }
}
