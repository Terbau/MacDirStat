//! Squarified cushion treemap (in the style of WinDirStat / KDirStat),
//! rendered on the CPU into a pixel buffer.

use crate::tree::Tree;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct R {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl R {
    fn w(&self) -> f64 {
        self.x1 - self.x0
    }
    fn h(&self) -> f64 {
        self.y1 - self.y0
    }
}

pub struct Treemap {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[u8; 4]>,
    /// Leaf node index drawn at each pixel (u32::MAX = none).
    pub ids: Vec<u32>,
    /// Pixel rectangle of every laid-out node that is at least ~1px.
    pub rects: HashMap<u32, R>,
}

const HEIGHT: f64 = 0.38;
const SCALE: f64 = 0.91;
const AMBIENT: f64 = 0.13;
const BRIGHT: f64 = 1.12;

struct Light {
    lx: f64,
    ly: f64,
    lz: f64,
}

impl Treemap {
    pub fn id_at(&self, x: usize, y: usize) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let id = self.ids[y * self.width + x];
        (id != u32::MAX).then_some(id)
    }
}

pub fn build(tree: &Tree, root: u32, width: usize, height: usize) -> Treemap {
    let mut tm = Treemap {
        width,
        height,
        pixels: vec![[40, 40, 40, 255]; width * height],
        ids: vec![u32::MAX; width * height],
        rects: HashMap::new(),
    };
    if width == 0 || height == 0 || tree.node(root).size == 0 {
        return tm;
    }
    let len = (1.0f64 + 1.0 + 100.0).sqrt();
    let light = Light {
        lx: -1.0 / len,
        ly: -1.0 / len,
        lz: 10.0 / len,
    };
    let rect = R {
        x0: 0.0,
        y0: 0.0,
        x1: width as f64,
        y1: height as f64,
    };
    let mut children_buf = Vec::new();
    layout(tree, root, rect, [0.0; 4], HEIGHT, &light, &mut tm, &mut children_buf);
    tm
}

fn add_ridge(r: &R, s: &mut [f64; 4], h: f64) {
    let w = r.w();
    let hh = r.h();
    if w <= 0.0 || hh <= 0.0 {
        return;
    }
    let h4 = 4.0 * h;
    let wf = h4 / w;
    s[2] += wf * (r.x1 + r.x0);
    s[0] -= wf;
    let hf = h4 / hh;
    s[3] += hf * (r.y1 + r.y0);
    s[1] -= hf;
}

#[allow(clippy::too_many_arguments)]
fn layout(
    tree: &Tree,
    idx: u32,
    rect: R,
    surface: [f64; 4],
    h: f64,
    light: &Light,
    tm: &mut Treemap,
    buf: &mut Vec<(u32, f64)>,
) {
    if rect.w() * rect.h() >= 1.0 {
        tm.rects.insert(idx, rect);
    }
    let mut s = surface;
    add_ridge(&rect, &mut s, h);

    let node = tree.node(idx);
    let too_small = rect.w() < 2.0 || rect.h() < 2.0;
    if !node.is_dir || node.child_count == 0 || too_small {
        let (color_idx, id) = if node.is_dir {
            (largest_file(tree, idx), idx)
        } else {
            (idx, idx)
        };
        render_leaf(tm, &rect, &s, tree.color(color_idx), id, light);
        return;
    }

    // Collect non-empty children with their target areas.
    let start = buf.len();
    let mut total = 0u64;
    for c in tree.children(idx) {
        let sz = tree.node(c).size;
        if sz > 0 {
            buf.push((c, sz as f64));
            total += sz;
        }
    }
    if total == 0 {
        buf.truncate(start);
        render_leaf(tm, &rect, &s, [160, 160, 160], idx, light);
        return;
    }
    let area = rect.w() * rect.h();
    let scale = area / total as f64;
    for item in &mut buf[start..] {
        item.1 *= scale;
    }
    buf[start..].sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let items: Vec<(u32, f64)> = buf.drain(start..).collect();

    let placed = squarify(&items, rect);
    for (c, r) in placed {
        layout(tree, c, r, s, h * SCALE, light, tm, buf);
    }
}

fn largest_file(tree: &Tree, mut idx: u32) -> u32 {
    loop {
        let n = tree.node(idx);
        if !n.is_dir {
            return idx;
        }
        match tree.children(idx).find(|&c| tree.node(c).size > 0) {
            Some(c) => idx = c,
            None => return idx,
        }
    }
}

fn worst(row_max: f64, row_min: f64, sum: f64, side: f64) -> f64 {
    let s2 = sum * sum;
    let w2 = side * side;
    (w2 * row_max / s2).max(s2 / (w2 * row_min))
}

fn squarify(items: &[(u32, f64)], rect: R) -> Vec<(u32, R)> {
    let mut out = Vec::with_capacity(items.len());
    let mut r = rect;
    let mut i = 0;
    let n = items.len();
    while i < n {
        let side = r.w().min(r.h());
        if side <= 0.0 {
            break;
        }
        let mut j = i;
        let mut sum = 0.0;
        let mut prev = f64::INFINITY;
        while j < n {
            let s = sum + items[j].1;
            let w = worst(items[i].1, items[j].1, s, side);
            if j > i && w > prev {
                break;
            }
            sum = s;
            prev = w;
            j += 1;
        }
        let last_row = j >= n;
        if r.w() >= r.h() {
            // column on the left
            let cw = if last_row { r.w() } else { sum / r.h() };
            let mut y = r.y0;
            for (k, item) in items[i..j].iter().enumerate() {
                let hk = if i + k + 1 == j { r.y1 - y } else { item.1 / cw };
                out.push((item.0, R { x0: r.x0, y0: y, x1: r.x0 + cw, y1: y + hk }));
                y += hk;
            }
            r.x0 += cw;
        } else {
            // row along the top
            let rh = if last_row { r.h() } else { sum / r.w() };
            let mut x = r.x0;
            for (k, item) in items[i..j].iter().enumerate() {
                let wk = if i + k + 1 == j { r.x1 - x } else { item.1 / rh };
                out.push((item.0, R { x0: x, y0: r.y0, x1: x + wk, y1: r.y0 + rh }));
                x += wk;
            }
            r.y0 += rh;
        }
        i = j;
    }
    out
}

fn render_leaf(tm: &mut Treemap, r: &R, s: &[f64; 4], color: [u8; 3], id: u32, light: &Light) {
    let x0 = (r.x0.round().max(0.0) as usize).min(tm.width);
    let x1 = (r.x1.round().max(0.0) as usize).min(tm.width);
    let y0 = (r.y0.round().max(0.0) as usize).min(tm.height);
    let y1 = (r.y1.round().max(0.0) as usize).min(tm.height);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (cr, cg, cb) = (color[0] as f64, color[1] as f64, color[2] as f64);
    let is = 1.0 - AMBIENT;
    for iy in y0..y1 {
        let ny = -(2.0 * s[1] * (iy as f64 + 0.5) + s[3]);
        let row = iy * tm.width;
        for ix in x0..x1 {
            let nx = -(2.0 * s[0] * (ix as f64 + 0.5) + s[2]);
            let mut cosa = (nx * light.lx + ny * light.ly + light.lz) / (nx * nx + ny * ny + 1.0).sqrt();
            if cosa > 1.0 {
                cosa = 1.0;
            }
            let mut p = is * cosa;
            if p < 0.0 {
                p = 0.0;
            }
            p = (p + AMBIENT) * BRIGHT;
            let px = &mut tm.pixels[row + ix];
            *px = [
                (cr * p).min(255.0) as u8,
                (cg * p).min(255.0) as u8,
                (cb * p).min(255.0) as u8,
                255,
            ];
            tm.ids[row + ix] = id;
        }
    }
}
