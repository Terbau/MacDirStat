//! Heuristics that find things that are usually safe (or worth reviewing) to delete.

use crate::tree::{age_days, format_age, Tree};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    /// Regenerable: caches, build output, dependencies.
    Safe,
    /// Probably reclaimable, but look before deleting.
    Review,
}

pub struct Candidate {
    pub node: u32,
    pub category: &'static str,
    pub reason: String,
    pub safety: Safety,
    pub size: u64,
    pub age_days: i64,
    pub selected: bool,
}

#[derive(Clone, Copy)]
enum Mode {
    Whole,
    EachChild,
}

/// (path relative to $HOME, category, reason, safety, mode)
const HOME_RULES: &[(&str, &str, &str, Safety, Mode)] = &[
    ("Library/Developer/Xcode/DerivedData", "Xcode DerivedData", "Xcode build products and indexes. Rebuilt automatically when you build.", Safety::Safe, Mode::EachChild),
    ("Library/Developer/Xcode/iOS DeviceSupport", "Xcode device support", "Debug symbols for an iOS version. Re-downloaded when you connect a device running it.", Safety::Safe, Mode::EachChild),
    ("Library/Developer/Xcode/watchOS DeviceSupport", "Xcode device support", "Debug symbols for a watchOS version. Re-downloaded when needed.", Safety::Safe, Mode::EachChild),
    ("Library/Developer/Xcode/tvOS DeviceSupport", "Xcode device support", "Debug symbols for a tvOS version. Re-downloaded when needed.", Safety::Safe, Mode::EachChild),
    ("Library/Developer/Xcode/Archives", "Xcode archives", "Archived app builds. Only needed to re-submit or symbolicate crashes of old releases.", Safety::Review, Mode::Whole),
    ("Library/Developer/CoreSimulator/Caches", "Simulator caches", "iOS Simulator caches. Recreated automatically.", Safety::Safe, Mode::Whole),
    ("Library/Developer/CoreSimulator/Devices", "Simulator devices", "Simulator devices and their app data. Prefer `xcrun simctl delete unavailable`.", Safety::Review, Mode::Whole),
    ("Library/Caches", "App cache", "Application cache. Apps recreate it when needed (quit the app first).", Safety::Safe, Mode::EachChild),
    (".cache", "Tool cache", "Cache directory used by command-line tools. Recreated when needed.", Safety::Safe, Mode::EachChild),
    ("Library/Logs", "Logs", "Log files. Safe to delete.", Safety::Safe, Mode::Whole),
    (".npm/_cacache", "npm cache", "npm download cache (same as `npm cache clean --force`).", Safety::Safe, Mode::Whole),
    (".npm/_npx", "npm cache", "Packages cached by npx.", Safety::Safe, Mode::Whole),
    ("Library/pnpm/store", "pnpm store", "pnpm package store. `pnpm store prune` removes only unused packages.", Safety::Review, Mode::Whole),
    (".pnpm-store", "pnpm store", "pnpm package store. `pnpm store prune` removes only unused packages.", Safety::Review, Mode::Whole),
    (".yarn/berry/cache", "Yarn cache", "Yarn package cache. Re-downloaded when needed.", Safety::Safe, Mode::Whole),
    (".bun/install/cache", "Bun cache", "Bun package cache. Re-downloaded when needed.", Safety::Safe, Mode::Whole),
    (".cargo/registry/cache", "Cargo cache", "Downloaded crate archives. Re-downloaded by cargo when needed.", Safety::Safe, Mode::Whole),
    (".cargo/registry/src", "Cargo cache", "Extracted crate sources. Re-extracted by cargo when needed.", Safety::Safe, Mode::Whole),
    (".cargo/git/checkouts", "Cargo cache", "Git dependency checkouts. Re-fetched by cargo when needed.", Safety::Safe, Mode::Whole),
    (".rustup/toolchains", "Rust toolchain", "Installed Rust toolchain. Remove unused ones with `rustup toolchain uninstall`.", Safety::Review, Mode::EachChild),
    (".gradle/caches", "Gradle cache", "Gradle dependency/build cache. Re-downloaded when needed.", Safety::Safe, Mode::Whole),
    (".gradle/wrapper/dists", "Gradle distribution", "Downloaded Gradle version. Re-downloaded by the wrapper when needed.", Safety::Safe, Mode::EachChild),
    (".m2/repository", "Maven cache", "Local Maven repository. Re-downloaded when needed.", Safety::Safe, Mode::Whole),
    ("go/pkg/mod", "Go module cache", "Go module cache (same as `go clean -modcache`).", Safety::Safe, Mode::Whole),
    (".nuget/packages", "NuGet cache", "NuGet package cache. Re-downloaded when needed.", Safety::Safe, Mode::Whole),
    (".cocoapods/repos", "CocoaPods specs", "CocoaPods spec repos. Re-cloned when needed.", Safety::Safe, Mode::Whole),
    (".android/avd", "Android emulator", "Android emulator image and its data.", Safety::Review, Mode::EachChild),
    ("Library/Android/sdk/system-images", "Android system image", "Android emulator system image. Re-downloadable via SDK manager.", Safety::Review, Mode::EachChild),
    (".Trash", "Trash", "Already in the Trash. Delete permanently (or empty the Trash) to free the space.", Safety::Safe, Mode::EachChild),
    ("Library/Containers/com.docker.docker/Data/vms", "Docker disk", "Docker's VM disk. Use `docker system prune -a` instead of deleting it.", Safety::Review, Mode::Whole),
    ("Library/Application Support/MobileSync/Backup", "iOS backup", "iPhone/iPad backup made by Finder. Delete old ones you no longer need.", Safety::Review, Mode::EachChild),
    (".ollama/models", "AI models", "Downloaded Ollama models. `ollama rm <model>` removes individual models.", Safety::Review, Mode::Whole),
    ("Library/Application Support/Code/Cache", "VS Code cache", "VS Code cache. Recreated automatically.", Safety::Safe, Mode::Whole),
    ("Library/Application Support/Code/CachedData", "VS Code cache", "VS Code cache. Recreated automatically.", Safety::Safe, Mode::Whole),
    ("Library/Application Support/Code/CachedExtensionVSIXs", "VS Code cache", "Cached extension installers.", Safety::Safe, Mode::Whole),
    ("Library/Application Support/Slack/Cache", "Slack cache", "Slack cache. Recreated automatically.", Safety::Safe, Mode::Whole),
    ("Library/Application Support/discord/Cache", "Discord cache", "Discord cache. Recreated automatically.", Safety::Safe, Mode::Whole),
    ("Library/Application Support/Google/Chrome/Default/Service Worker/CacheStorage", "Chrome cache", "Website caches stored by Chrome.", Safety::Safe, Mode::Whole),
];

/// Project-level build/dependency directories: (dir name, required sibling markers (any), category, reason, safety)
const DIR_RULES: &[(&str, &[&str], &str, &str, Safety)] = &[
    ("node_modules", &[], "Node dependencies", "Installed npm packages. Reinstall with `npm install` / `pnpm install` / `yarn`.", Safety::Safe),
    ("target", &["Cargo.toml"], "Rust build output", "Cargo build output (same as `cargo clean`).", Safety::Safe),
    ("target", &["pom.xml"], "Maven build output", "Maven build output (same as `mvn clean`).", Safety::Safe),
    ("build", &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"], "Gradle build output", "Gradle build output (same as `gradle clean`).", Safety::Safe),
    ("build", &["CMakeLists.txt"], "CMake build output", "CMake build directory. Regenerated by building.", Safety::Safe),
    ("build", &["package.json"], "JS build output", "Built bundle. Regenerated by the build script.", Safety::Safe),
    ("build", &["pubspec.yaml"], "Flutter build output", "Flutter build output (same as `flutter clean`).", Safety::Safe),
    ("build", &["setup.py", "pyproject.toml"], "Python build output", "Python build artifacts.", Safety::Safe),
    (".gradle", &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"], "Gradle project cache", "Per-project Gradle cache.", Safety::Safe),
    (".cxx", &["build.gradle", "build.gradle.kts"], "Android NDK build", "Native (NDK) build intermediates.", Safety::Safe),
    (".externalNativeBuild", &[], "Android NDK build", "Native (NDK) build intermediates.", Safety::Safe),
    ("dist", &["package.json"], "JS build output", "Built bundle. Regenerated by the build script.", Safety::Safe),
    ("dist", &["setup.py", "pyproject.toml"], "Python build output", "Built wheels / sdists.", Safety::Safe),
    (".next", &[], "Next.js build cache", "Next.js build output and cache.", Safety::Safe),
    (".nuxt", &[], "Nuxt build output", "Nuxt build output.", Safety::Safe),
    (".output", &["package.json"], "JS build output", "Framework build output.", Safety::Safe),
    (".svelte-kit", &[], "SvelteKit build output", "SvelteKit generated files.", Safety::Safe),
    (".turbo", &[], "Turborepo cache", "Turborepo cache.", Safety::Safe),
    (".parcel-cache", &[], "Parcel cache", "Parcel bundler cache.", Safety::Safe),
    (".angular", &[], "Angular cache", "Angular CLI cache.", Safety::Safe),
    (".expo", &["package.json"], "Expo cache", "Expo local cache.", Safety::Safe),
    (".vite", &[], "Vite cache", "Vite dependency cache.", Safety::Safe),
    (".build", &["Package.swift"], "SwiftPM build output", "Swift Package Manager build output.", Safety::Safe),
    ("Pods", &["Podfile"], "CocoaPods", "Installed pods. Reinstall with `pod install`.", Safety::Safe),
    ("DerivedData", &[], "Xcode DerivedData", "Xcode build products. Rebuilt automatically.", Safety::Safe),
    ("__pycache__", &[], "Python bytecode", "Compiled Python bytecode. Regenerated automatically.", Safety::Safe),
    (".pytest_cache", &[], "Python tool cache", "pytest cache.", Safety::Safe),
    (".mypy_cache", &[], "Python tool cache", "mypy cache.", Safety::Safe),
    (".ruff_cache", &[], "Python tool cache", "ruff cache.", Safety::Safe),
    (".tox", &[], "Python tool cache", "tox environments.", Safety::Safe),
    (".nox", &[], "Python tool cache", "nox environments.", Safety::Safe),
    (".dart_tool", &["pubspec.yaml"], "Dart tool cache", "Dart/Flutter tool cache.", Safety::Safe),
    (".stack-work", &[], "Haskell build output", "Stack build output.", Safety::Safe),
    ("dist-newstyle", &[], "Haskell build output", "Cabal build output.", Safety::Safe),
    (".zig-cache", &[], "Zig cache", "Zig build cache.", Safety::Safe),
    ("zig-cache", &[], "Zig cache", "Zig build cache.", Safety::Safe),
    ("zig-out", &["build.zig"], "Zig build output", "Zig build output.", Safety::Safe),
    ("_build", &["mix.exs"], "Elixir build output", "Mix build output.", Safety::Safe),
    ("deps", &["mix.exs"], "Elixir dependencies", "Mix dependencies. Re-fetch with `mix deps.get`.", Safety::Safe),
    (".terraform", &[], "Terraform cache", "Terraform providers/modules. Re-fetch with `terraform init`.", Safety::Safe),
    ("bin", &["*.csproj", "*.fsproj"], ".NET build output", ".NET build output (same as `dotnet clean`).", Safety::Safe),
    ("obj", &["*.csproj", "*.fsproj"], ".NET build output", ".NET intermediate build files.", Safety::Safe),
];

const INSTALLER_EXTS: &[&str] = &["dmg", "pkg", "mpkg", "iso", "xip", "ipsw"];
const ARCHIVE_EXTS: &[&str] = &["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar"];

const MIN_SIZE: u64 = 1_000_000;

fn has_marker(tree: &Tree, parent: u32, marker: &str) -> bool {
    if let Some(suffix) = marker.strip_prefix('*') {
        tree.children(parent).any(|c| tree.node(c).name.ends_with(suffix))
    } else {
        tree.child_named(parent, marker).is_some()
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn find(tree: &Tree) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut claimed: HashSet<u32> = HashSet::new();
    let push = |out: &mut Vec<Candidate>, node: u32, cat: &'static str, reason: String, safety: Safety| {
        let n = tree.node(node);
        if n.size >= MIN_SIZE {
            out.push(Candidate {
                node,
                category: cat,
                reason,
                safety,
                size: n.size,
                age_days: age_days(n.mtime),
                selected: false,
            });
        }
    };

    let home = home_dir();
    let mut downloads = None;
    if let Some(home) = &home {
        downloads = tree.find_path(&home.join("Downloads"));
        for (rel, cat, reason, safety, mode) in HOME_RULES {
            let Some(idx) = tree.find_path(&home.join(rel)) else {
                continue;
            };
            if claimed.iter().any(|&c| tree.is_ancestor_or_self(c, idx)) {
                continue;
            }
            claimed.insert(idx);
            match mode {
                Mode::Whole => push(&mut out, idx, cat, reason.to_string(), *safety),
                Mode::EachChild => {
                    for c in tree.children(idx) {
                        push(&mut out, c, cat, reason.to_string(), *safety);
                    }
                }
            }
        }
    }

    // Walk the whole tree looking for project build dirs and big stale files.
    let mut stack = vec![0u32];
    while let Some(idx) = stack.pop() {
        for c in tree.children(idx) {
            if claimed.contains(&c) {
                continue;
            }
            let n = tree.node(c);
            if n.is_dir {
                let mut matched = false;
                for (name, markers, cat, reason, safety) in DIR_RULES {
                    if &*n.name == *name && (markers.is_empty() || markers.iter().any(|m| has_marker(tree, idx, m))) {
                        let age = age_days(n.mtime);
                        let reason = if age >= 30 {
                            format!("{reason} Project last modified {}.", format_age(age))
                        } else {
                            reason.to_string()
                        };
                        push(&mut out, c, cat, reason, *safety);
                        matched = true;
                        break;
                    }
                }
                // Python virtualenvs
                if !matched && tree.child_named(c, "pyvenv.cfg").is_some() {
                    push(&mut out, c, "Python virtualenv", "Python virtual environment. Recreate it from requirements when needed.".into(), Safety::Review);
                    matched = true;
                }
                if !matched && n.size >= MIN_SIZE {
                    stack.push(c);
                }
            } else if n.size >= 50_000_000 {
                let ext = tree.ext_name(c);
                let age = age_days(n.mtime);
                let in_downloads = downloads.is_some_and(|d| tree.is_ancestor_or_self(d, c));
                if INSTALLER_EXTS.contains(&ext) {
                    push(&mut out, c, "Installer / disk image", format!("Installer or disk image (.{ext}). Usually not needed after installing. Modified {}.", format_age(age)), Safety::Review);
                } else if in_downloads && (n.size >= 200_000_000 || ARCHIVE_EXTS.contains(&ext)) {
                    push(&mut out, c, "Large download", format!("Large file in Downloads, modified {}.", format_age(age)), Safety::Review);
                } else if n.size >= 1_000_000_000 && age >= 180 {
                    push(&mut out, c, "Large old file", format!("Large file, last modified {}.", format_age(age)), Safety::Review);
                }
            }
        }
    }

    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}
