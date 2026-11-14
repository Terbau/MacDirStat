# MacDirStat

A fast WinDirStat-style disk usage viewer for macOS, written in Rust (egui).

- **Cushion treemap**: every file is a rectangle sized by its disk usage and colored by file type.
- **Cleanup Suggestions**: build output (node_modules, target, .next, DerivedData, Pods, build, …),
  caches (~/Library/Caches, npm/pnpm/cargo/gradle/…), Xcode/simulator leftovers, installers,
  and large old files. Marked **SAFE** (regenerable) or **REVIEW** (look before deleting).
- **Folders** tree and **File Types** list.
- Move to Trash or delete permanently (with confirmation). Right-click any item to Reveal in Finder.

## Run

```bash
./build_app.sh            # builds dist/MacDirStat.app
open dist/MacDirStat.app
./build_app.sh --install  # also copies it to /Applications
```

Or straight from the terminal: `cargo run --release -- ~/some/folder`.
Headless summary: `target/release/macdirstat --report ~`.

For complete results, grant the app **Full Disk Access** in
System Settings → Privacy & Security → Full Disk Access.

## Treemap controls
- Hover: show path and size. Click: select (it's highlighted in the Folders tree).
- Double-click: zoom into that folder. **Up** / **Top** zoom out.
- Right-click: Reveal in Finder, Open, Copy Path, Trash, Delete.
