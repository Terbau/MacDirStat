//! Fast parallel filesystem scanner.

use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::tree::{ExtStat, FlatNode, Tree, NO_EXT};

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

pub(crate) struct TmpNode {
    pub(crate) name: Box<str>,
    pub(crate) size: u64,
    pub(crate) files: u64,
    pub(crate) mtime: i64,
    pub(crate) is_dir: bool,
    pub(crate) children: Vec<TmpNode>,
}

struct Ctx<'a> {
    progress: &'a Progress,
    hardlinks: Mutex<HashSet<(u64, u64)>>,
    skip: Vec<PathBuf>,
}

/// Paths that should never be descended into (virtual / duplicate mounts).
fn default_skips(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        "/System/Volumes",
        "/Volumes",
        "/dev",
        "/net",
        "/home",
        "/private/var/vm",
        "/private/var/folders", // temp; often permission-denied noise
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    // If the user explicitly scans one of the skipped paths, allow it.
    v.retain(|p| !root.starts_with(p));
    v
}

struct RawEntry {
    name: Box<str>,
    is_dir: bool,
    /// Allocated size on disk (what actually consumes space).
    size: u64,
    mtime: i64,
    nlink: u32,
    dev: u64,
    ino: u64,
}

fn list_dir(path: &Path) -> std::io::Result<Vec<RawEntry>> {
    match bulk::list_dir(path) {
        Ok(v) => Ok(v),
        Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) || e.raw_os_error() == Some(libc::EINVAL) => {
            list_dir_std(path)
        }
        Err(e) => Err(e),
    }
}

/// Portable fallback using readdir + lstat.
fn list_dir_std(path: &Path) -> std::io::Result<Vec<RawEntry>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(path)?.flatten() {
        let Ok(md) = entry.metadata() else { continue };
        out.push(RawEntry {
            name: entry.file_name().to_string_lossy().into(),
            is_dir: md.file_type().is_dir(),
            size: md.blocks() * 512,
            mtime: md.mtime(),
            nlink: md.nlink() as u32,
            dev: md.dev(),
            ino: md.ino(),
        });
    }
    Ok(out)
}

/// macOS getattrlistbulk(2): fetches names + metadata for many directory
/// entries per syscall, which is much faster than readdir + lstat per file.
mod bulk {
    use super::RawEntry;
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    unsafe extern "C" {
        fn getattrlistbulk(dirfd: libc::c_int, alist: *mut AttrList, buf: *mut libc::c_void, size: libc::size_t, options: u64) -> libc::c_int;
    }

    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_DEVID: u32 = 0x0000_0002;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
    const ATTR_CMN_FILEID: u32 = 0x0200_0000;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    const VDIR: u32 = 2;

    thread_local! {
        static BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; 256 * 1024]);
    }

    #[inline]
    fn rd_u32(b: &[u8], off: usize) -> u32 {
        u32::from_ne_bytes(b[off..off + 4].try_into().unwrap())
    }
    #[inline]
    fn rd_i32(b: &[u8], off: usize) -> i32 {
        i32::from_ne_bytes(b[off..off + 4].try_into().unwrap())
    }
    #[inline]
    fn rd_u64(b: &[u8], off: usize) -> u64 {
        u64::from_ne_bytes(b[off..off + 8].try_into().unwrap())
    }

    pub fn list_dir(path: &Path) -> std::io::Result<Vec<RawEntry>> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut al = AttrList {
            bitmapcount: 5,
            reserved: 0,
            commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_ERROR | ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE | ATTR_CMN_MODTIME | ATTR_CMN_FILEID,
            volattr: 0,
            dirattr: 0,
            fileattr: ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE,
            forkattr: 0,
        };
        let mut out = Vec::new();
        let res = BUF.with(|cell| {
            let mut buf = cell.borrow_mut();
            loop {
                let n = unsafe { getattrlistbulk(fd, &mut al, buf.as_mut_ptr() as *mut _, buf.len(), 0) };
                if n < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if n == 0 {
                    return Ok(());
                }
                let b = &buf[..];
                let mut pos = 0usize;
                for _ in 0..n {
                    let len = rd_u32(b, pos) as usize;
                    if len == 0 || pos + len > b.len() {
                        break;
                    }
                    let entry = &b[pos..pos + len];
                    pos += len;
                    let mut f = 4usize;
                    let ret_common = rd_u32(entry, f);
                    let ret_file = rd_u32(entry, f + 12);
                    f += 20;
                    if ret_common & ATTR_CMN_ERROR != 0 {
                        let err = rd_u32(entry, f);
                        f += 4;
                        if err != 0 {
                            continue;
                        }
                    }
                    let mut name: Box<str> = "".into();
                    if ret_common & ATTR_CMN_NAME != 0 {
                        let off = rd_i32(entry, f);
                        let nlen = rd_u32(entry, f + 4) as usize;
                        let start = (f as isize + off as isize) as usize;
                        if start + nlen <= entry.len() && nlen > 0 {
                            let raw = &entry[start..start + nlen - 1]; // strip NUL
                            name = String::from_utf8_lossy(raw).into();
                        }
                        f += 8;
                    }
                    let mut dev = 0u64;
                    if ret_common & ATTR_CMN_DEVID != 0 {
                        dev = rd_i32(entry, f) as u32 as u64;
                        f += 4;
                    }
                    let mut objtype = 0u32;
                    if ret_common & ATTR_CMN_OBJTYPE != 0 {
                        objtype = rd_u32(entry, f);
                        f += 4;
                    }
                    let mut mtime = 0i64;
                    if ret_common & ATTR_CMN_MODTIME != 0 {
                        mtime = rd_u64(entry, f) as i64;
                        f += 16;
                    }
                    let mut ino = 0u64;
                    if ret_common & ATTR_CMN_FILEID != 0 {
                        ino = rd_u64(entry, f);
                        f += 8;
                    }
                    let mut nlink = 1u32;
                    let mut size = 0u64;
                    if ret_file & ATTR_FILE_LINKCOUNT != 0 {
                        nlink = rd_u32(entry, f);
                        f += 4;
                    }
                    if ret_file & ATTR_FILE_ALLOCSIZE != 0 {
                        size = rd_u64(entry, f);
                    }
                    if name.is_empty() {
                        continue;
                    }
                    out.push(RawEntry { name, is_dir: objtype == VDIR, size, mtime, nlink, dev, ino });
                }
            }
        });
        unsafe { libc::close(fd) };
        res.map(|_| out)
    }
}

fn scan_dir(path: &Path, name: Box<str>, mtime: i64, ctx: &Ctx) -> TmpNode {
    let p = ctx.progress;
    let mut node = TmpNode {
        name,
        size: 0,
        files: 0,
        mtime,
        is_dir: true,
        children: Vec::new(),
    };
    if p.cancel.load(Ordering::Relaxed) {
        return node;
    }
    let dirs_seen = p.dirs.fetch_add(1, Ordering::Relaxed);
    if dirs_seen % 512 == 0 {
        if let Ok(mut c) = p.current.lock() {
            *c = path.display().to_string();
        }
    }

    let entries = match list_dir(path) {
        Ok(e) => e,
        Err(_) => {
            p.errors.fetch_add(1, Ordering::Relaxed);
            return node;
        }
    };

    let mut subdirs: Vec<(PathBuf, Box<str>, i64)> = Vec::new();
    let (mut nfiles, mut nbytes) = (0u64, 0u64);
    for e in entries {
        if e.is_dir {
            let child_path = path.join(&*e.name);
            if ctx.skip.iter().any(|s| s == &child_path) {
                continue;
            }
            subdirs.push((child_path, e.name, e.mtime));
        } else {
            let mut size = e.size;
            if e.nlink > 1 {
                if !ctx.hardlinks.lock().unwrap().insert((e.dev, e.ino)) {
                    size = 0; // hard link already counted elsewhere
                }
            }
            nfiles += 1;
            nbytes += size;
            node.children.push(TmpNode {
                name: e.name,
                size,
                files: 1,
                mtime: e.mtime,
                is_dir: false,
                children: Vec::new(),
            });
        }
    }
    p.files.fetch_add(nfiles, Ordering::Relaxed);
    p.bytes.fetch_add(nbytes, Ordering::Relaxed);

    let sub: Vec<TmpNode> = subdirs
        .into_par_iter()
        .map(|(cp, n, mt)| scan_dir(&cp, n, mt, ctx))
        .collect();
    node.children.extend(sub);

    for c in &node.children {
        node.size += c.size;
        node.files += c.files;
        node.mtime = node.mtime.max(c.mtime);
    }
    node
}

pub struct ScanResult {
    pub tree: Tree,
    pub elapsed: std::time::Duration,
}

pub fn scan(root: PathBuf, progress: Arc<Progress>) -> ScanResult {
    let start = Instant::now();
    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .num_threads(std::env::var("MDS_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get() * 2).unwrap_or(8)))
        .build()
        .expect("thread pool");

    let ctx = Ctx {
        progress: &progress,
        hardlinks: Mutex::new(HashSet::new()),
        skip: default_skips(&root),
    };
    let root_mtime = fs::symlink_metadata(&root).map(|m| m.mtime()).unwrap_or(0);
    let root_name: Box<str> = root.display().to_string().into();
    let tmp = pool.install(|| scan_dir(&root, root_name, root_mtime, &ctx));
    let tree = flatten(tmp, root);
    ScanResult {
        tree,
        elapsed: start.elapsed(),
    }
}

/// Convert the owned tree into a flat arena where each node's children are
/// contiguous and sorted by size (largest first).
pub(crate) fn flatten(root: TmpNode, root_path: PathBuf) -> Tree {
    let mut nodes: Vec<FlatNode> = Vec::with_capacity(1024);
    let mut ext_map: HashMap<Box<str>, u16> = HashMap::new();
    let mut exts: Vec<ExtStat> = Vec::new();

    let mut ext_of = |name: &str, size: u64, exts: &mut Vec<ExtStat>| -> u16 {
        let ext = match name.rfind('.') {
            Some(i) if i > 0 && i + 1 < name.len() && name.len() - i <= 12 => {
                name[i + 1..].to_ascii_lowercase()
            }
            _ => return NO_EXT,
        };
        let idx = if let Some(&i) = ext_map.get(ext.as_str()) {
            i
        } else {
            if exts.len() >= (NO_EXT as usize - 1) {
                return NO_EXT;
            }
            let i = exts.len() as u16;
            ext_map.insert(ext.clone().into(), i);
            exts.push(ExtStat {
                name: ext.into(),
                bytes: 0,
                count: 0,
                color: [128, 128, 128],
            });
            i
        };
        exts[idx as usize].bytes += size;
        exts[idx as usize].count += 1;
        idx
    };

    let make = |n: &TmpNode, parent: u32, ext: u16| FlatNode {
        name: n.name.clone(),
        size: n.size,
        files: n.files,
        mtime: n.mtime,
        parent,
        first_child: 0,
        child_count: 0,
        is_dir: n.is_dir,
        ext,
        deleted: false,
    };

    nodes.push(make(&root, u32::MAX, NO_EXT));
    let mut queue: VecDeque<(u32, Vec<TmpNode>)> = VecDeque::new();
    queue.push_back((0, root.children));
    while let Some((idx, mut children)) = queue.pop_front() {
        children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        let first = nodes.len() as u32;
        nodes[idx as usize].first_child = first;
        nodes[idx as usize].child_count = children.len() as u32;
        for (i, mut c) in children.into_iter().enumerate() {
            let ext = if c.is_dir {
                NO_EXT
            } else {
                ext_of(&c.name, c.size, &mut exts)
            };
            nodes.push(make(&c, idx, ext));
            if c.is_dir && !c.children.is_empty() {
                let grand = std::mem::take(&mut c.children);
                queue.push_back((first + i as u32, grand));
            }
        }
    }

    let mut tree = Tree {
        nodes,
        exts,
        root_path,
    };
    tree.assign_colors();
    tree
}
