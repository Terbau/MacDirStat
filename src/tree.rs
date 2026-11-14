//! Flat arena representation of the scanned file tree.

use std::path::{Path, PathBuf};

pub const NO_EXT: u16 = u16::MAX;

pub struct FlatNode {
    pub name: Box<str>,
    pub size: u64,
    pub files: u64,
    /// For directories: newest modification time anywhere in the subtree.
    pub mtime: i64,
    pub parent: u32,
    pub first_child: u32,
    pub child_count: u32,
    pub is_dir: bool,
    pub ext: u16,
    pub deleted: bool,
}

pub struct ExtStat {
    pub name: Box<str>,
    pub bytes: u64,
    pub count: u64,
    pub color: [u8; 3],
}

pub struct Tree {
    pub nodes: Vec<FlatNode>,
    pub exts: Vec<ExtStat>,
    pub root_path: PathBuf,
}

const PALETTE: [[u8; 3]; 16] = [
    [0, 0, 255],
    [255, 0, 0],
    [0, 220, 0],
    [0, 230, 230],
    [255, 0, 255],
    [255, 230, 0],
    [120, 120, 255],
    [255, 140, 0],
    [0, 160, 100],
    [200, 120, 255],
    [255, 120, 160],
    [140, 200, 0],
    [0, 140, 255],
    [190, 90, 40],
    [100, 200, 180],
    [230, 180, 120],
];

impl Tree {
    pub fn children(&self, idx: u32) -> impl Iterator<Item = u32> + '_ {
        let n = &self.nodes[idx as usize];
        (n.first_child..n.first_child + n.child_count).filter(|&c| !self.nodes[c as usize].deleted)
    }

    pub fn node(&self, idx: u32) -> &FlatNode {
        &self.nodes[idx as usize]
    }

    pub fn path(&self, idx: u32) -> PathBuf {
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = idx;
        while cur != 0 {
            let n = &self.nodes[cur as usize];
            parts.push(&n.name);
            cur = n.parent;
        }
        let mut p = self.root_path.clone();
        for part in parts.iter().rev() {
            p.push(part);
        }
        p
    }

    pub fn ancestors(&self, idx: u32) -> Vec<u32> {
        let mut v = Vec::new();
        let mut cur = self.nodes[idx as usize].parent;
        while cur != u32::MAX {
            v.push(cur);
            cur = self.nodes[cur as usize].parent;
        }
        v
    }

    pub fn is_ancestor_or_self(&self, anc: u32, idx: u32) -> bool {
        let mut cur = idx;
        while cur != u32::MAX {
            if cur == anc {
                return true;
            }
            cur = self.nodes[cur as usize].parent;
        }
        false
    }

    pub fn child_named(&self, idx: u32, name: &str) -> Option<u32> {
        self.children(idx).find(|&c| &*self.nodes[c as usize].name == name)
    }

    pub fn find_path(&self, path: &Path) -> Option<u32> {
        let rel = path.strip_prefix(&self.root_path).ok()?;
        let mut cur = 0u32;
        for comp in rel.components() {
            let s = comp.as_os_str().to_str()?;
            cur = self.child_named(cur, s)?;
        }
        Some(cur)
    }

    pub fn ext_name(&self, idx: u32) -> &str {
        let e = self.nodes[idx as usize].ext;
        if e == NO_EXT { "" } else { &self.exts[e as usize].name }
    }

    pub fn color(&self, idx: u32) -> [u8; 3] {
        let n = &self.nodes[idx as usize];
        if n.is_dir {
            return [160, 160, 160];
        }
        if n.ext == NO_EXT {
            [170, 170, 170]
        } else {
            self.exts[n.ext as usize].color
        }
    }

    pub fn assign_colors(&mut self) {
        let mut order: Vec<usize> = (0..self.exts.len()).collect();
        order.sort_by(|&a, &b| self.exts[b].bytes.cmp(&self.exts[a].bytes));
        for (rank, &i) in order.iter().enumerate() {
            self.exts[i].color = if rank < PALETTE.len() {
                PALETTE[rank]
            } else {
                // muted hash-based color for the long tail
                let h = self.exts[i]
                    .name
                    .bytes()
                    .fold(2166136261u32, |a, b| (a ^ b as u32).wrapping_mul(16777619));
                hsv_to_rgb((h % 360) as f32, 0.35, 0.75)
            };
        }
    }

    /// Mark a node as deleted and subtract its size from all ancestors.
    pub fn remove(&mut self, idx: u32) {
        if idx == 0 || self.nodes[idx as usize].deleted {
            return;
        }
        let size = self.nodes[idx as usize].size;
        let files = self.nodes[idx as usize].files;
        self.nodes[idx as usize].deleted = true;
        let ext = self.nodes[idx as usize].ext;
        if ext != NO_EXT {
            let e = &mut self.exts[ext as usize];
            e.bytes = e.bytes.saturating_sub(size);
            e.count = e.count.saturating_sub(1);
        }
        for a in self.ancestors(idx) {
            let n = &mut self.nodes[a as usize];
            n.size = n.size.saturating_sub(size);
            n.files = n.files.saturating_sub(files);
        }
    }

    pub fn is_deleted(&self, idx: u32) -> bool {
        let mut cur = idx;
        while cur != u32::MAX {
            if self.nodes[cur as usize].deleted {
                return true;
            }
            cur = self.nodes[cur as usize].parent;
        }
        false
    }
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ]
}

pub fn format_size(b: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0; // macOS Finder uses decimal units
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn age_days(mtime: i64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ((now - mtime) / 86400).max(0)
}

pub fn format_age(days: i64) -> String {
    if days < 1 {
        "today".into()
    } else if days < 60 {
        format!("{days} d ago")
    } else if days < 730 {
        format!("{} mo ago", days / 30)
    } else {
        format!("{} yr ago", days / 365)
    }
}
