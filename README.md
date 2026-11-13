# Projects — Live Stats

A native desktop dashboard for `P:\Projects`, built with [Rune](https://github.com/anomalyco/rune)
(dev rev `14eb362e4e56aadf9b64c82640417ab0c9da5fe0`) — GPU-rendered, no web view.

Mirrors the `stats-inline.html` mock: KPI row, squarified disk-footprint treemap, language mix
bars, a project-pulse donut, a momentum scatter (recency × size × footprint), build-junk
composition bars and insight cards.

## Run

```sh
cargo run -p projects-stats                 # live scan of P:\Projects (override: RUNE_STATS_ROOT)
cargo run -p projects-stats -- --scene demo # deterministic fixture, no scanning
```

The window opens and a background scan starts immediately; projects fill in as they finish.
It auto-rescans every 5 minutes, and the Rescan button forces one. The scan walks every folder
in parallel (8 workers, hidden files included � `.git` history is real disk usage), buckets
bytes into source / `node_modules` / `target` / other build output (`.git` lands in other),
counts hand-written lines by extension (code files < 256 KiB only), and asks git for commits,
last-commit age and dirty-file counts. Measured on this machine: 88 folders, 82.0 GiB
(verified against robocopy's 88,039,170,366 bytes), 2.97 M LOC in ~13 s.

## Footprint

- Frame CPU p50 2.4 ms / p95 3.0 ms (headless, 768 instances) against a 16.6 ms budget
- Idle: 0.0% CPU � Rune draws nothing when nothing changes; gauge commits only fire when a
  displayed fps/ram value actually changes
- 128 MB working set at rest, ~175 MB peak while scanning
- Footer shows live `fps � MB � scan` in mono, sampled once a second

## Check headlessly

```sh
cargo test -p projects-stats                                       # 10 tests incl. layout + scan
cargo test -p projects-stats real_root_scan_smoke -- --ignored --nocapture  # real scan, timed
cargo run -p projects-stats -- --scene demo --snap snap.png --tree tree.json
```

`--tree` dumps the element tree (keys, rects, text, tooltips); `--snap` renders a PNG.
`--size 1100x2500` shows the whole scrollable page in one snapshot.

## Layout

- `src/scan.rs` — parallel walker + git facts, streaming `ScanEvent`s over a channel
- `src/main.rs` — state, poll loop (drains events, auto-rescan), tests
- `src/ui.rs` — the view: header, KPIs, cards, charts
- `src/charts.rs` — squarified treemap + donut/circle geometry, tests
- `src/model.rs` — `Project`, categories, colours, formatting
- `src/fixture.rs` — the demo scene's data

## Notes

- Rune's `[patch]` entries don't cross workspaces, so `Cargo.toml` re-patches
  `wgpu-hal`, `winit` and `parley_engine` to the rune clone's `vendor/` copies.
- Toolchain pinned to 1.99.1 (rune's `rust-toolchain.toml`).
