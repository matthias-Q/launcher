# Rune port of `launch` — findings

Headless rebuild of the Freya launcher (`/home/mqueitsch/repos/launch`) on
[rune](https://github.com/anomalyco/rune), verified entirely offscreen (snapshots, element
trees, scripted input, unit tests) plus a cold-start benchmark against the original.

Branch `rune-experiment`. The workspace keeps the untouched original (`launch`) next to the
port (`rune-launch`) for A/B comparison.

## What was ported

- **All seven modules**: Calc (math evaluator), Apps (freedesktop `.desktop` scan), Run
  (`$PATH` scan), Windows (EWMH list/focus/close over x11rb), Emoji, Power, and dmenu. The
  clipboard module is read-only (see deltas).
- **Scoring identical**: every module ranks through the same nucleo-matcher with the same
  weights as launch (`score / 2` keywords, +64 exact-name, 24/16/8 recency boosts), and the
  "All" tab merges with launch's module order and strong-before-fuzzy tiers. `calc.rs`,
  `run.rs`, `power.rs`, `emoji.rs` entries, `icons.rs`, `windows.rs`, `clipboard/store.rs`
  are ported near-verbatim; `entries.rs` swaps Freya `Bytes` for `Vec<u8>`.
- **Glass/blur reproduced** (`config.blur` > 0): captures the desktop under the pointer's
  monitor (x11rb, before the window maps, on a thread overlapping the module scan), draws it
  behind the panel, and the panel's rune GPU `backdrop_blur` + `blur_opacity` tint frosts it —
  the config fields (`blur`, `blur_opacity`, `tint`, `window_radius`) get their launch meaning
  back. `--scene glass` injects a synthetic desktop so the effect is verifiable offscreen.
  The frost is clearly visible at launch's default `blur_opacity = 0.45`; a high `blur_opacity`
  with a dark `tint` (e.g. `0.75` + `#181921`) covers it under the tint, so it reads as a
  plain panel — `docs/10-glass.png` (dark, buried) vs `docs/11-glass-visible.png` (frosted).
- **Config schema identical** (serde, `deny_unknown_fields`, same defaults, same `EXAMPLE`
  text for `--default-config`).
- **CLI identical**: `--config`, `--mode`, `--default-config`, `--version`, and dmenu
  (`-d/--index/--multi/--no-custom/-p`), same `requires`/`conflicts` rules.
- **Keyboard model identical**: Enter (Shift+Enter = keep open / dmenu mark), arrows, PgUp/
  PgDn, Tab (cycle when >1 tab), Alt+1..9 jump, Escape, `/`-style Ctrl+d / Shift+Delete
  remove, confirm-arm on `confirm` entries.
- **Host abstraction** ready for the missed daemon/warm-start port: prints are captured,
  `exit(code)` stores the code and only kills the process outside headless runs, and
  side-effecting actions (spawn/copy/focus) are skipped headless so snapshots and scripts are
  deterministic. Activation correctness is covered by unit tests with a recording host.

## Verification (all headless)

- **69 tests green** — 37 original `launch` unchanged, 32 new for the port (Runtime-driven
  key handling: filtering, clamp, arrows, Tab/Alt, Enter/shift, multi-mark, keep-open,
  confirm-air two-step, escape, dmenu index + custom text, calc result; plus backdrop
  BGRX→RGBA downscale + synthetic desktop).
- **Snapshots** (`--snap`): default All tab, `fire` (merged Apps/Windows/Emoji rows, 🔥
  glyphs render), calc (`2^10 / 3` → `341.3333333333`), power, Run tab (PATH executables),
  and `glass` (translucent frosted panel over the synthetic desktop, rounded corners, sharp
  desktop fragments showing through the tint). Rounded corners + transparency confirmed with
  a compositor present.
- **Element trees** (`--tree`) keyed as `launcher/tabs/tab-0..5`, `input-row/query`,
  `result-0..7`.
- **Scripted input** (`--script` + `--at mark:`): typed `fire` then Down×2 → selection lands
  on the third row; a scripted Enter picks the match in dmenu mode.
- **dmenu semantics** verified end-to-end on the real binary: `--index` prints the pick
  index, plain mode prints the line, no match prints the raw query, `--no-custom`
  suppresses it, `--multi` plus Shift+Enter prints every marked line in order.

## Cold-start benchmark

Same real user config (`~/.config/launch/config.toml`), cold process each run, warm
OS/driver caches, 7 runs, median + MAD.

| Launcher | Median | MAD | Method |
|---|---|---|---|
| `rune-launch` (release) | **109 ms** | 4 ms | `--first-pixels --frames 1`: first GPU-complete offscreen pixels (gpu init 53 ms, first frame 25 ms, readback 9 ms) |
| `launch` original (release) | **302 ms** | 10 ms | window mapped on `:0` (`xdotool` search poll) |

Caveats: the endpoints differ — rune measures offscreen GPU pixels (includes module scans,
which block the first frame), launch measures an X window-map (includes WM round-trips, and
polling adds some noise); neither covers daemon warm-start (not ported). Both are fresh
process cold starts, which is the main interaction cost for a popup launcher.

**Result: the rune port reaches first visible pixels roughly 2.8× faster than the original
Freya launch, with the same modules, scoring, config, and dmenu behavior.**

## Deltas vs launch

1. **Clipboard recording daemon not ported.** `backend`/`daemon`/`secret`/`service` are
   omitted; the module reads/list/clears history written by the original launch's daemon
   (`cliphist` namespaces kept). Documented in `rune-launch/src/clipboard/mod.rs`.
2. **No per-app daemon/warm-start port.** The rune port is the standalone launcher; the
   Host abstraction keeps the door open.
3. **Headless determinism.** Side-effecting actions are no-ops under rune headless tooling;
   the dmenu pick is still printed (it is the observable contract).
4. **Glass differs in one detail.** launch blurs its capture on the CPU and aligns it 1:1
   under the window; the port lets rune blur on the GPU (`backdrop_blur`) and fits the
   capture, so the desktop shows through the panel a fraction off from 1:1 — invisible behind
   the blur (default `blur = 10`), and the corners between panel and window edge are sharp
   desktop, as launch's are.
5. Everything else matches: default module order, tab counts, placeholders, borderless
   transparent window with rounded corners under a compositor.

## Reproduce

```
# build
cargo build --release -p rune-launch    # from the worktree root
# tests
cargo test --workspace
# headless verification
target/debug/rune-launch --list-scenes
script -qec "target/debug/rune-launch --scene fire --snap docs/02-fire.png" /dev/null
target/debug/rune-launch --scene glass --snap docs/10-glass.png   # frosted panel, no X11 capture
printf 'apple\nbanana\n' | target/debug/rune-launch --dmenu --index \
  --script scripts/dmenu.json --at mark:picked --snap docs/08-dmenu.png
# benchmark
python3 scripts/bench-rune.py --exe target/release/rune-launch --runs 7
python3 scripts/bench-launch.py --exe target/release/launch --runs 7   # needs X :0
```

The `scripts/` directory holds the JSON input scripts and the two benchmark harnesses;
`docs/` holds the snapshots, trees, and `benchmark-*.json` raw results.
