//! Synthetic file tree used for screenshots (`macdirstat --demo`).
//! Nothing is read from (or written to) disk.

use crate::scan::{TmpNode, flatten};
use crate::tree::Tree;
use std::path::PathBuf;

pub const HOME: &str = "/Users/demo";

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn pick<'a>(&mut self, s: &[&'a str]) -> &'a str {
        s[(self.next() % s.len() as u64) as usize]
    }
}

struct Builder {
    root: TmpNode,
    now: i64,
    rng: Rng,
}

fn new_node(name: &str, is_dir: bool) -> TmpNode {
    TmpNode {
        name: name.into(),
        size: 0,
        files: 0,
        mtime: 0,
        is_dir,
        children: Vec::new(),
    }
}

fn dir_mut<'a>(mut cur: &'a mut TmpNode, path: &str) -> &'a mut TmpNode {
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let pos = match cur.children.iter().position(|c| c.is_dir && &*c.name == part) {
            Some(p) => p,
            None => {
                cur.children.push(new_node(part, true));
                cur.children.len() - 1
            }
        };
        cur = &mut cur.children[pos];
    }
    cur
}

impl Builder {
    fn file(&mut self, path: &str, size: u64, age_days: i64) {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let mtime = self.now - age_days * 86400;
        let mut f = new_node(name, false);
        f.size = size;
        f.files = 1;
        f.mtime = mtime;
        dir_mut(&mut self.root, dir).children.push(f);
    }

    fn dir(&mut self, path: &str) {
        dir_mut(&mut self.root, path);
    }

    /// Scatter `n` files (heavy-tailed sizes summing to ~`total`) below `dir`,
    /// in up to `depth` levels of subfolders named from `subdirs`.
    #[allow(clippy::too_many_arguments)]
    fn spray(&mut self, dir: &str, n: usize, total: u64, exts: &[&str], stems: &[&str], subdirs: &[&str], depth: u32, max_age: i64) {
        let weights: Vec<f64> = (0..n).map(|_| self.rng.f().powf(4.0) + 0.002).collect();
        let sum: f64 = weights.iter().sum();
        for (i, w) in weights.iter().enumerate() {
            let mut p = dir.to_string();
            let levels = if subdirs.is_empty() { 0 } else { self.rng.next() % (depth as u64 + 1) };
            for _ in 0..levels {
                p.push('/');
                p.push_str(self.rng.pick(subdirs));
            }
            let stem = self.rng.pick(stems);
            let ext = self.rng.pick(exts);
            let name = if ext.is_empty() { format!("{stem}{i}") } else { format!("{stem}{i}.{ext}") };
            let size = ((w / sum) * total as f64) as u64 + 512;
            let age = (self.rng.f() * max_age as f64) as i64;
            self.file(&format!("{p}/{name}"), size, age);
        }
    }
}

fn finalize(n: &mut TmpNode) {
    if !n.is_dir {
        return;
    }
    let (mut size, mut files, mut mtime) = (0, 0, n.mtime);
    for c in &mut n.children {
        finalize(c);
        size += c.size;
        files += c.files;
        mtime = mtime.max(c.mtime);
    }
    n.size = size;
    n.files = files;
    n.mtime = mtime;
}

pub fn build() -> Tree {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut b = Builder { root: new_node(HOME, true), now, rng: Rng(0x9E3779B97F4A7C15) };

    // ---- Library -------------------------------------------------------
    let dd = "Library/Developer/Xcode/DerivedData";
    let build_exts = ["o", "o", "swiftmodule", "pcm", "dylib", "swiftdoc", "d", "dia", "json"];
    let build_stems = ["Module", "ViewModel", "Service", "Model", "View", "Store", "Network"];
    let build_dirs = ["Build", "Intermediates.noindex", "Products", "Index.noindex", "Debug-iphonesimulator", "ModuleCache.noindex"];
    for (name, size) in [("Pebble-gkqhwbxzyfyxnr", 6 * GB), ("WeatherKitDemo-azcvlqjtpxpo", 3 * GB), ("Playground-dmzxkwqgtnab", 1_400 * MB)] {
        b.spray(&format!("{dd}/{name}"), 600, size, &build_exts, &build_stems, &build_dirs, 3, 40);
    }
    let ds = "Library/Developer/Xcode/iOS DeviceSupport";
    for (name, size, age) in [("iPhone15,2 17.5.1 (21F90)", 5_800 * MB, 420), ("iPhone15,2 18.1 (22B83)", 6_100 * MB, 30)] {
        b.spray(&format!("{ds}/{name}/Symbols"), 250, size, &["dylib", "dylib", "framework", ""], &["libswift", "CoreUI", "UIKitCore", "libsystem"], &["usr/lib", "System/Library/Frameworks", "System/Library/PrivateFrameworks"], 2, age);
    }
    for (uuid, size) in [("6F3A1C2E-8B4D-4E2A-9C1F-0D7B5A3E2F14", 4_200 * MB), ("B9D2E7F0-3A6C-4F81-8E25-7C4A1B9D0E63", 3_100 * MB), ("1E8C4B7A-D5F2-4A93-B6E0-92F3C8D1A5B7", 2_300 * MB)] {
        b.spray(&format!("Library/Developer/CoreSimulator/Devices/{uuid}/data"), 300, size, &["sqlite", "png", "plist", "db", "dat"], &["store", "cache", "img", "prefs"], &["Containers", "Library", "Media", "Documents"], 3, 90);
    }
    b.file("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw", 31_800 * MB, 2);
    b.spray("Library/Caches/com.spotify.client", 200, 3_200 * MB, &["", "file"], &["data", "chunk"], &["Data", "Storage"], 1, 20);
    b.spray("Library/Caches/Homebrew", 60, 2_100 * MB, &["gz", "bottle", "json"], &["node", "python", "ffmpeg", "llvm"], &["downloads"], 1, 120);
    b.spray("Library/Caches/Google", 80, 1_400 * MB, &["", "dat"], &["f_", "data_"], &["Chrome/Default/Cache"], 1, 10);
    b.spray("Library/Caches/pip", 120, 900 * MB, &["whl", ""], &["torch", "numpy", "pandas"], &["http-v2", "wheels"], 2, 200);
    b.spray("Library/Caches/com.apple.Safari", 90, 600 * MB, &["db", ""], &["Cache", "fs"], &["WebKitCache"], 1, 7);
    b.spray("Library/Application Support/Steam/steamapps/common/Starfall Odyssey", 140, 18 * GB, &["pak", "pak", "bundle", "dylib", "ogg"], &["Content", "Audio", "Textures", "Shaders"], &["Data", "Data/Streaming", "Bin"], 2, 300);
    b.spray("Library/Application Support/MobileSync/Backup/00008110-001A2B3C4D5E6F70", 900, 14 * GB, &[""], &["3d0d7e5fb2ce288813306e4d4636395e047a3d28", "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"], &["00", "1a", "3d", "7f", "b2", "e9"], 1, 260);
    b.spray("Library/Application Support/Slack/Cache", 120, 800 * MB, &[""], &["data_", "f_"], &["Cache_Data"], 1, 14);
    b.spray("Library/Application Support/Code/CachedData", 60, 700 * MB, &["code", "js"], &["chunk", "bundle"], &["a1b2c3", "d4e5f6"], 1, 30);
    b.spray("Library/Mail/V10", 1500, 3_100 * MB, &["emlx", "emlx", "pdf", "jpg"], &["msg", "attachment"], &["INBOX.mbox", "Archive.mbox", "Sent.mbox"], 2, 900);
    b.spray("Library/Logs", 80, 400 * MB, &["log", "ips", "diag"], &["system", "crash", "install"], &["DiagnosticReports", "CoreSimulator"], 1, 60);

    // ---- Media ---------------------------------------------------------
    b.spray("Pictures/Photos Library.photoslibrary/originals", 3500, 41 * GB, &["heic", "heic", "heic", "jpg", "mov", "png"], &["IMG_", "IMG_E"], &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F"], 1, 1500);
    b.spray("Pictures/Photos Library.photoslibrary/resources/derivatives", 2000, 4 * GB, &["jpg"], &["thumb_"], &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F"], 1, 1500);
    b.file("Movies/Iceland Road Trip 4K.mov", 9_400 * MB, 610);
    b.file("Movies/Wedding Highlights.mp4", 4_100 * MB, 820);
    b.file("Movies/Screen Recording 2025-02-11.mov", 2_300 * MB, 270);
    b.spray("Movies/Iceland Road Trip.fcpbundle", 120, 7_900 * MB, &["mov", "mov", "fcpevent", "png"], &["clip", "render", "proxy"], &["Original Media", "Render Files", "Transcoded Media"], 1, 600);
    b.spray("Music/Music/Media.localized/Music", 1400, 12 * GB, &["m4a", "m4a", "mp3", "aiff"], &["Track "], &["Aurora Lane", "The Paper Kites", "Nordic Echoes", "Velvet Static", "Moonlit Harbor", "Glass Animals"], 2, 2000);
    b.spray("Music/Logic", 90, 3_600 * MB, &["wav", "aif", "logicx"], &["Bounce", "Vocals", "Synth"], &["Demo Song.logicx/Media", "Beat Tape/Audio Files"], 1, 400);

    // ---- Downloads / Documents -------------------------------------------
    b.file("Downloads/Xcode_16.1.xip", 3_400 * MB, 300);
    b.file("Downloads/ubuntu-24.04-desktop-arm64.iso", 3_100 * MB, 210);
    b.file("Downloads/Docker.dmg", 620 * MB, 95);
    b.file("Downloads/dataset-2024.zip", 2_400 * MB, 150);
    b.file("Downloads/Figma.dmg", 280 * MB, 40);
    b.spray("Downloads", 70, 1_100 * MB, &["pdf", "png", "jpg", "zip", "docx"], &["invoice", "Screenshot ", "report", "scan"], &[], 0, 400);
    b.spray("Documents", 900, 7_600 * MB, &["pdf", "pdf", "docx", "pages", "key", "xlsx", "numbers", "png"], &["Report", "Notes", "Budget", "Slides", "Contract", "Thesis"], &["Work", "Personal", "Taxes", "School", "Projects", "Archive"], 2, 1800);
    b.spray("Desktop", 40, 900 * MB, &["png", "pdf", "mov"], &["Screenshot ", "draft"], &[], 0, 60);

    // ---- dev projects ----------------------------------------------------
    let js = ["js", "js", "js", "mjs", "cjs", "map", "ts", "json", "node", "wasm"];
    let pkgs = ["react", "next", "typescript", "esbuild", "@swc", "lodash", "webpack", "eslint", "@babel", "prettier", "zod", "vite", "rollup", "sharp", "@img"];
    for (proj, nm, next) in [("web-dashboard", 1_250 * MB, 650 * MB), ("landing-page", 480 * MB, 220 * MB)] {
        b.file(&format!("dev/{proj}/package.json"), 2_000, 30);
        b.spray(&format!("dev/{proj}/src"), 120, 3 * MB, &["tsx", "ts", "css"], &["page", "component", "hook", "util"], &["app", "components", "lib"], 2, 30);
        b.spray(&format!("dev/{proj}/node_modules"), 3000, nm, &js, &["index", "chunk", "dist", "lib"], &pkgs, 3, 120);
        b.spray(&format!("dev/{proj}/.next"), 300, next, &["js", "pack", "json", "map"], &["chunk", "webpack", "page"], &["cache/webpack", "server", "static/chunks"], 2, 30);
    }
    b.file("dev/rust-cli/Cargo.toml", 900, 12);
    b.spray("dev/rust-cli/src", 30, 400_000, &["rs"], &["main", "parse", "render", "config"], &[], 0, 12);
    b.spray("dev/rust-cli/target", 1600, 7_400 * MB, &["rlib", "rlib", "rmeta", "o", "d", ""], &["libserde-", "libtokio-", "libsyn-", "librayon-", "libclap-", "rust_cli-"], &["debug/deps", "release/deps", "debug/incremental", "debug/build", "release/build"], 2, 12);
    b.file("dev/ios-app/Podfile", 700, 50);
    b.spray("dev/ios-app/Pods", 900, 950 * MB, &["swift", "h", "m", "a", "framework"], &["Firebase", "Alamofire", "Lottie", "Realm"], &["Firebase", "Alamofire", "lottie-ios", "Realm"], 2, 50);
    b.spray("dev/ios-app/Pebble", 200, 180 * MB, &["swift", "png", "xcassets"], &["View", "Model", "icon"], &["Views", "Models", "Assets.xcassets"], 2, 20);
    b.file("dev/ml-experiments/pyproject.toml", 1_200, 70);
    b.file("dev/ml-experiments/.venv/pyvenv.cfg", 300, 70);
    b.spray("dev/ml-experiments/.venv/lib/python3.12/site-packages", 2200, 2_800 * MB, &["py", "py", "so", "dylib", "pyc"], &["__init__", "core", "ops", "_C"], &["torch", "numpy", "pandas", "scipy", "transformers", "sklearn"], 2, 70);
    b.spray("dev/ml-experiments/data", 40, 9_200 * MB, &["parquet", "csv", "npy"], &["train", "val", "test", "features"], &["raw", "processed"], 1, 240);
    b.spray("dev/ml-experiments/checkpoints", 12, 6_300 * MB, &["pt", "safetensors"], &["epoch_", "model_"], &[], 0, 230);
    b.spray("dev/ml-experiments/src/__pycache__", 30, 3 * MB, &["pyc"], &["train", "model", "data"], &[], 0, 70);
    b.file("dev/android-app/build.gradle.kts", 1_500, 90);
    b.spray("dev/android-app/build", 700, 2_300 * MB, &["dex", "class", "apk", "jar", "bin"], &["classes", "R", "app-debug", "merged"], &["intermediates", "outputs/apk", "tmp/kotlin-classes"], 2, 90);
    b.spray("dev/android-app/.gradle", 100, 420 * MB, &["bin", "lock", ""], &["fileHashes", "executionHistory"], &["8.7"], 1, 90);
    b.spray("dev/android-app/app/src", 150, 40 * MB, &["kt", "xml", "png"], &["Main", "Activity", "layout"], &["main/java", "main/res"], 2, 90);
    b.file("dev/api-server/go.mod", 400, 5);
    b.spray("dev/api-server", 60, 2 * MB, &["go"], &["handler", "server", "db"], &["internal", "cmd"], 1, 5);

    // ---- tool caches -----------------------------------------------------
    b.spray(".npm/_cacache", 1500, 2_100 * MB, &[""], &["sha512-"], &["content-v2/sha512", "index-v5"], 2, 200);
    b.spray(".cargo/registry/cache", 400, 1_300 * MB, &["crate"], &["serde-", "tokio-", "syn-", "windows-"], &["index.crates.io-6f17d22bba15001f"], 1, 300);
    b.spray(".cargo/registry/src", 1500, 2_400 * MB, &["rs", "toml", "md"], &["lib", "mod"], &["index.crates.io-6f17d22bba15001f"], 2, 300);
    b.spray(".rustup/toolchains/stable-aarch64-apple-darwin", 200, 1_400 * MB, &["rlib", "dylib", ""], &["libstd-", "librustc_driver-"], &["lib/rustlib", "bin"], 2, 40);
    b.spray(".rustup/toolchains/nightly-2024-06-01-aarch64-apple-darwin", 200, 1_500 * MB, &["rlib", "dylib", ""], &["libstd-", "librustc_driver-"], &["lib/rustlib", "bin"], 2, 520);
    b.spray(".gradle/caches", 800, 3_600 * MB, &["jar", "bin", "pom"], &["kotlin-stdlib-", "gradle-", "okhttp-"], &["modules-2/files-2.1", "transforms-4", "8.7"], 2, 150);
    b.spray(".ollama/models/blobs", 6, 8_500 * MB, &[""], &["sha256-6a0746a1ec1aef3e7ec53868f220ff6e389f6f8ef87a01d77c96807de94ca2aa"], &[], 0, 100);

    // ---- Trash ------------------------------------------------------------
    b.spray(".Trash/old-prototype", 300, 1_200 * MB, &["js", "png", "json"], &["bundle", "asset"], &["node_modules", "assets"], 2, 30);
    b.file(".Trash/Screen Recording 2025-06-02.mov", 900 * MB, 20);

    b.dir("Public");
    let mut root = b.root;
    finalize(&mut root);
    flatten(root, PathBuf::from(HOME))
}
