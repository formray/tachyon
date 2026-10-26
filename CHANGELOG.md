# Changelog

## v1.0.1

Hardening for public use: safer for everyone, easier for anyone. No new readings; everything from 1.0.0 works as before.

**Easier to use**
- **First start:** defaults with no personal paths, and the panel opens with a short hint on where to find Settings. In a folder you cannot write to, `tachyon.toml` goes to `%LOCALAPPDATA%\Tachyon` instead.
- **Damaged settings:** a `tachyon.toml` that cannot be read (syntax, wrong types, not UTF-8, over 1 MB) is kept as `tachyon.toml.corrupt-<date>-<time>`; Tachyon starts with the defaults and says so.
- **No GPU readings:** plain messages for no NVIDIA driver, an old driver and a driver reset (TDR). After a reset Tachyon reconnects every 2 s for a minute, then every 10 s, and keeps running.
- **One instance:** starting it again brings the running one to the front. `TACHYON_CONFIG` and `TACHYON_DATA_DIR` let a second copy run with its own settings and data.
- **Settings:** *Start with Windows* (a shortcut in the Startup folder; off by default, no registry), a GPU picker when there is more than one, *Reset settings* (two clicks, keeps the old file as `.bak` and the window positions), and where the data lives and how to remove Tachyon.
- **Keyboard:** Tab, Shift+Tab, Space, Enter, arrows, Ctrl+Tab and Esc in Settings, with a focus ring; Ctrl+, opens Settings.
- **Monitors:** windows left on a monitor that was unplugged come back to the nearest screen; the island and panel stop drawing while a fullscreen app is in front, as Hex already did.
- **Crash log:** if Tachyon itself panics it writes `%LOCALAPPDATA%\Tachyon\logs\panic-<time>.txt`, shows a short message and exits; the next start mentions it.

**Safer**
- The agent API's shared memory and the single-instance mutex allow only the current user (and SYSTEM), at medium integrity; `tachyon mcp` refuses a section created by another account.
- Run-time DLLs are loaded from System32 only.
- Size caps and damage tolerance for everything read from disk: render logs, the flight recorder and its `session.json` (which can no longer name a file outside the recorder folder), `jobs.jsonl` (one damaged line no longer hides the jobs after it), the crash report and the event log XML (a panic on some non-ASCII attributes is fixed). MCP: 1 MB per request line, 64 per batch, invalid UTF-8 answered with an error instead of stopping the server.
- Randomised and mutated input tests for the settings loader, render log scanner, job history, flight recorder, crash report and MCP server.
- [SECURITY.md](SECURITY.md) and a privacy section in the README.

**Builds**
- CI runs `cargo deny` and `cargo audit`; all actions pinned by commit SHA with Dependabot updates; job and step timeouts (the 1.0.0 release run hung on apt); Rust build cache.
- Releases carry a signed build-provenance attestation for `tachyon.exe` and the zip, next to `SHA256SUMS.txt`.
- winget and Scoop manifests in `packaging/` (not submitted yet).

## v1.0.0

Tachyon 1.0 is the first release. It supersedes the 5.x preview builds: those numbers were development iterations, and versioning restarts here. Everything from the previews is in, plus four things that explain what the GPU is doing, worked out from the readings Tachyon already takes (no extra polling, nothing leaves the PC).

- **Flight recorder and crash report:** the last 15 minutes of readings in two fixed-size ring files (192-byte checksummed records, written every 5 s, flushed every 30 s). After an unexpected restart or a killed session, the next start builds a report: what happened just before, the peaks, the process holding the most VRAM, a per-minute timeline and the matching Windows events (Kernel-Power 41, bug checks, unexpected shutdowns, WHEA hardware errors, NVIDIA driver resets) in plain words. Hex announces a restart once with *Open report*. *Export flight recorder* writes CSV and JSON. Settings: `blackbox`, `blackbox_minutes`.
- **"Why is it slow?":** one sentence on what is holding the work back (GPU, power cap, heat, video decoder or encoder, one CPU thread, all cores, VRAM, PCIe), with *sure*, *likely* or *possibly* and the evidence. A new answer must hold for four evaluations before it replaces the old one. In the panel (*Why? ›*) and Hex's bubble (*Why is it slow? ›*).
- **VRAM forecast:** Theil-Sen trend over two minutes for the card and each process; a warning only when the trend is steady and the card would be full within 15 minutes; idle background apps holding VRAM.
- **Job history:** finished render jobs and GPU-busy sessions in `%LOCALAPPDATA%\Tachyon\history\jobs.jsonl` with duration, load, fps, energy (driver energy counter, or integrated power), peak VRAM and temperature, throttled time and main limit, compared with the previous run of the same job. History view in the panel. Optional `price_per_kwh` and `currency`.
- **`tachyon.exe mcp`:** a local MCP server (stdio, protocol 2025-06-18 and earlier) with seven read-only tools for AI agents. Off unless `agent_api = true`; the running app then publishes its status in shared memory every two ticks.
- **Settings page:** *Settings…* in the right-click menu (island, Hex, panel) or the gear in the panel. Five tabs: General (mode, Hex announcements, bubble close time, refresh rate), Projects (render folders with a folder picker), Alerts (temperature and VRAM thresholds, forecast horizon), Data (flight recorder, history, energy price and currency) and AI agents (agent API with its live state, *Copy MCP config* with the full exe path). Values are checked, saved as you change them and applied live; only the flight recorder needs a restart. Saves go through a temporary file and a rename, keep `tachyon.toml.bak` and preserve comments and unknown keys. New settings: `hex_announce`, `bubble_close_secs`, `vram_forecast_minutes`.
- **VRAM used matches `nvidia-smi`:** read with `nvmlDeviceGetMemoryInfo_v2`, which leaves out the memory the driver reserves (about 300 MiB on a 16 GB card); older drivers fall back to the first version.
- **Job history energy:** a job whose first reading was quiet now still uses the driver's energy counter instead of integrating power.
- New menu items: *Crash report*, *Job history*, *Export flight recorder*, *Settings…*.
- Exe about 1.85 MB (was 1.4 MB).

Measured on Windows 11 with an RTX 5080: about 0.05 % of total CPU and 32 MB working set in Hex mode during a GPU render, 0.025 % and 25 MB idle; `tachyon.exe mcp` answers in under a millisecond with Tachyon running. Figures and method in the README.

Known limits: checked on one Windows 11 PC so far; the crash report after a real restart, its Windows events and the restart detection are tested with synthetic data only (wine does not answer event log queries). See the README.

## Previews

Preview builds before 1.0. Their tags and downloads stay on GitHub for reference.

### v5.1.1

- Fixed: on Windows a render log still held open by its writer could look stale to the scan; its last-write time is now read from the file itself, and the scan tests no longer depend on Windows keeping file times when copying (the failed v5.1.0 Windows CI run).

### v5.1.0

Tachyon 5.1 brings **Hex**, a desktop companion, alongside the new **Aurora** look.

- **Hex companion:** a small glass hexagon with eyes that sits on your desktop. It breathes and blinks, draws a glowing progress line round its edge during a render, warms up (with a little steam) when the card is hot and dozes off after ten quiet minutes. Click it for a bubble with render progress, load, temperature, the biggest VRAM user and four key numbers, or open the full panel from there.
- **Modes:** island, companion or panel, switched from the right-click menu and remembered (`mode = ...` in `tachyon.toml`).
- **Aurora glass:** frosted glass with a live colour field that follows GPU load and temperature; truly borderless windows with per-pixel alpha, a soft shadow and click-through around the shapes. The island fills like a progress bar during a render; the panel sizes itself to its content.
- **GPU process attribution** with category chips, launcher chains and per-process SM/ENC/DEC and VRAM.
- **Still light:** no constant animation timer; about 0.3 % of one core idle under wine (real Windows figures to follow). One portable exe of about 1.4 MB.
- `glass = true/false` from 4.x settings files still loads.

Known limits: not yet run on real Windows; no real backdrop blur (`glass = "blur"` is experimental); NVIDIA only. See the README for the full list.
