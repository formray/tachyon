# Contributing

Thanks for helping. A few ground rules keep Tachyon small and trustworthy.

## Principles
- **Read-only, always.** Nothing is written except the app's own `tachyon.toml`. Never change clocks, fans, power limits or other processes. No network access, no telemetry.
- **Light first.** No constant animation timers (Hex arms one one-shot timer per drawn frame; transitions are bounded), no extra threads, no GPU rendering context. Present a frame only when something changed. If a change adds per-frame work, include `TACHYON_DEBUG=1` paint/CPU numbers before and after.
- **Readable.** Text colours must keep WCAG 4.5:1 on the glass and solid cards (`cargo test` checks `theme.rs`); labels stay at 12 px or more.
- **Privacy by default.** Show file names, not full paths. Anything more revealing (like command lines) must be opt-in in the toml.
- **Graceful degradation.** Every NVML call may be missing or return NOT_SUPPORTED; show "N/A" and keep going.

## Workflow
1. `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings` and `cargo test --release` must pass (tests run on any OS; Windows-only code lives in `src/gui.rs`, `src/gui/`, `src/present.rs`, `src/dw.rs` and `procinfo::win`).
2. `cargo clippy --release --target x86_64-pc-windows-gnu --all-targets -- -D warnings` must pass too. CI runs all of this, plus an MSVC build on Windows.
3. Keep the Windows UI in plain windows-sys + tiny-skia + DirectWrite (no UI frameworks).
4. New parsers need a small anonymised fixture in `tests/samples/` and a test. Never commit real logs with personal paths, project names or client names.
5. For UI changes, attach a screenshot. You can make one without Windows using wine:
   - build the fake driver: `x86_64-w64-mingw32-gcc -O2 -shared -o nvml.dll tests/fake_nvml_v3.c -lm` (add `-DFORCE_THR` to force limiters) and copy it to the prefix's `windows/system32/`;
   - the scripts expect the prefix in `/tmp/tachyon-wine/wp`, `nvml.dll` and `sleeper.exe` in `/tmp/tachyon-work`, and a wallpaper made by `tests/make_wallpaper.py /tmp/tachyon-work/wall_demo.jpg`;
   - fake GPU processes: build `tests/sleeper.c`; `tests/wine.sh start` starts Xvfb, picom (so layered alpha is really blended) and copies of the sleeper inside the prefix, and writes their PIDs to `C:\rmfake\procs.txt` (`<pid> <vram_mb|-1> <G|C> <sm> <enc> <dec>` per line);
   - `tests/shots.sh <scenario>` takes screenshots (a fake render log tree goes in `/tmp/tachyon-wine/projects/demo4k`); `tests/interact.sh` and `tests/interact_hex.sh` click through island, panel, Hex and bubble with xdotool; `tests/measure.sh` measures CPU and RSS of each mode;
   - put static (not variable) TrueType fonts in the prefix's `windows/Fonts`, or wine's DirectWrite picks synthetic oblique faces;
   - `TACHYON_DUMP_DIR=<dir>` writes the raw premultiplied BGRA frames, for previews; `cargo run --example hex_dump` renders Hex frames without Windows.

## Reporting bugs
Please include your Windows version and build, GPU, driver version and the `TACHYON_DEBUG=1` output (it contains no personal data beyond process names, which you can redact).
