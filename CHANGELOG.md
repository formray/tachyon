# Changelog

## v1.0.0

Tachyon 1.0 is the first release. It supersedes the 5.x preview builds: those numbers were development iterations, and versioning restarts here. Everything from the previews is in, plus four things that explain what the GPU is doing, worked out from the readings Tachyon already takes (no extra polling, nothing leaves the PC).

- **Flight recorder and crash report:** the last 15 minutes of readings in two fixed-size ring files (192-byte checksummed records, written every 5 s, flushed every 30 s). After an unexpected restart or a killed session, the next start builds a report: what happened just before, the peaks, the process holding the most VRAM, a per-minute timeline and the matching Windows events (Kernel-Power 41, bug checks, unexpected shutdowns, WHEA hardware errors, NVIDIA driver resets) in plain words. Hex announces a restart once with *Open report*. *Export flight recorder* writes CSV and JSON. Settings: `blackbox`, `blackbox_minutes`.
- **"Why is it slow?":** one sentence on what is holding the work back (GPU, power cap, heat, video decoder or encoder, one CPU thread, all cores, VRAM, PCIe), with *sure*, *likely* or *possibly* and the evidence. A new answer must hold for four evaluations before it replaces the old one. In the panel (*Why? ›*) and Hex's bubble (*Why is it slow? ›*).
- **VRAM forecast:** Theil-Sen trend over two minutes for the card and each process; a warning only when the trend is steady and the card would be full within 15 minutes; idle background apps holding VRAM.
- **Job history:** finished render jobs and GPU-busy sessions in `%LOCALAPPDATA%\Tachyon\history\jobs.jsonl` with duration, load, fps, energy (driver energy counter, or integrated power), peak VRAM and temperature, throttled time and main limit, compared with the previous run of the same job. History view in the panel. Optional `price_per_kwh` and `currency`.
- **`tachyon.exe mcp`:** a local MCP server (stdio, protocol 2025-06-18 and earlier) with seven read-only tools for AI agents. Off unless `agent_api = true`; the running app then publishes its status in shared memory every two ticks.
- New menu items: *Crash report*, *Job history*, *Export flight recorder*.
- Exe about 1.8 MB (was 1.4 MB).

Known limits: still not run on real Windows; Windows events, the energy counter and restart detection are tested with synthetic data only (wine does not answer event log queries). See the README.

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
