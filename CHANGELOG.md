# Changelog

## v1.3.0 — 2026-10-29

Hex, the companion, is redrawn (Tachy v5) and gets two new states. Everything else works as in 1.2.0, still read-only.

**New look: Tachy v5**
- A cleaner hexagon with no outline and calm, shape-driven eyes: the eye shape alone tells the state, even at 1x.
- Ambient motion around him again, drawn as part of the sprite: sparkles while rendering, heat waves when hot, a sweat drop when VRAM is full, z's when sleepy, a happy blink now and then at idle.
- Two built-in skins: **Velvet** (default, a soft glowing glass) and **Aurora flat** (a flat gradient with a fine grain). Both are embedded atlases at 1x, 1.5x and 2x (`res/hex/velvet/`, `res/hex/flat/`), drawn by the same compositor.

**Companion skin setting**
- Settings > General > *Companion skin*: Velvet / Aurora flat, applies at once (no restart). In `tachyon.toml`: `hex_skin = "velvet"` or `"flat"`.
- Older settings files load unchanged (Velvet); an unknown value falls back to Velvet instead of failing. *Reset settings* goes back to Velvet.

**Two new states: VRAM full and alarm**
- **VRAM full:** on at 92 % or more of VRAM for about 3 s, off below 85 %. Amber colours, a glance at a small sweat drop that slides down.
- **Alarm:** on a new crash report (an unexpected restart or GPU driver failure), a GPU driver reset in the last minute, or the red temperature (with 2 °C hysteresis). Red, with a pulse, a small jolt, trembling eyes and '!' ticks. It stays at least 10 s; after a crash report until you click Hex or the next render starts.
- Priority when several apply: alarm > hot > VRAM full > render > sleepy > idle. Hot keeps its amber threshold, now with 2 °C hysteresis.

**Preview flags**
- `tachyon.exe --hex-demo` cycles through the six states, 8 s each; `tachyon.exe --hex-state=NAME` holds one (`idle`, `render` or `busy`, `hot`, `vram`, `alarm`, `sleepy`). A click on Hex goes to the next state. Preview mode always starts as Hex, uses made-up readings, records nothing (no flight recorder, history or fast sampling) and does not change the saved start mode.
- `tachyon.exe --version` prints the version and exits.

**Build and tests**
- 203 tests on Linux (198 in 1.2.0): state priority, the VRAM and temperature thresholds with their hysteresis, crash and driver-error alarms with the 10 s minimum and click / next-render acknowledgement, both atlases loading with all six states and every frame they use, the skin setting (default, round trip, unknown values).
- The app compositor matches the approved v5 design renders within one level out of 255 on every frame of all six states, the blinks and the transitions, on both skins and all three scales (324 frames).
- Tested on Windows 11 with an RTX 5080 alongside a running 1.2.0: all six states on both skins, `--hex-demo`, the live skin switch in Settings, the MCP server (same twelve tools) and `tachyon mark`. About 1 % of one core while animating, 28 MB private memory.

## v1.2.0 — 2026-10-29

Four additions for render machines, all read-only like before, plus clearer crash and job reports.

**A Tachyon-only stop is diagnosed from what happened before it**
- When Tachyon stops but the PC keeps running (same boot), only evidence up to the stop counts. Events logged later (an app crashing ten minutes after) go to a separate `after_events` list ("After the stop · not related") and are never evidence. After a restart, application crashes logged after the stop are set apart the same way; the restart's own reports at the next boot still count.
- No RAM, BIOS, PSU or GPU-overclock causes for a Tachyon-only stop without direct evidence (WHEA hardware errors or GPU driver errors just before): the RAM speed, the BIOS start time and the clock belong to the boot, not to this stop.
- Without a crash of Tachyon (its crash log or a Windows Application Error for tachyon.exe) the verdict is "Tachyon was closed at 21:38. Your PC kept running.", a neutral note, not shown on the main view. A real crash of Tachyon reads "Tachyon crashed at ..."; a GPU driver error just before points to the driver.
- Reports saved by an older version are revised on load with these rules (the 26 Oct 21:38 report, which blamed later CC_Engine_x64.exe crashes and EXPO/BIOS from the earlier boot, now reads "closed, nothing to do").
- New `Report::verdict()` (what happened, is the PC OK, what to do) and `severity()` (note / warning / critical).

**Layout: verdict first, details on demand**
- Crash report: one-sentence verdict, then the top cause as a card with plain words (EXPO/XMP explained) and one action; other causes folded; evidence, peaks and timeline behind one row. Times in their own column, text wrapped (no ellipsis), at most three bullets per block, numbers and units aligned in the peaks grid, colour only for meaning (neutral note, amber warning, red critical). Events after the stop shown apart. Sections give way to "in the export" lines when the screen is short (checked at 100, 125 and 150 % scaling).
- New Job report view (menu "Last marked job report", "Job report ›" on the main view and in History): verdict, the numbers in an aligned grid (GPU, power, VRAM, energy, temperature, power-cap and heat-limit time), the PCIe link in one sentence, VRAM by process with bars (three shown, the rest folded), clock-limit reasons and earlier jobs folded.
- Main view: one type scale (12.5 body, 14 values and verdicts, 9.5 labels), wrapped sentences, the limiter shows its first reason "+N" instead of a cut word, a PCIe link row (neutral when full or saving power at idle, amber when slow), process rows drop whole details that do not fit instead of cutting them. The session timer row gave way to the PCIe link.

**Status, comparisons and plain words**
- One status scale for every verdict (`status.rs`): all fine (green dot), worth a look (amber), act now (red), always with its words ("ALL FINE") next to the dot. Crash reports, job reports, the pre-render check, the bottleneck line and the PCIe link use the same mapping.
- Cause shares are percentages only when two or more causes compete (the second has 20 % or more and the top one is under 80 %); otherwise a word: certain, likely or possible. No share bar for a single cause. The voice line follows the same rule.
- The job report compares with the previous run of the same job only (same name, run counters ignored; never a different job): speed, average GPU, VRAM peak, average power and energy as deltas, or "First run, nothing to compare." Also in History and in `get_job_reports` (`vs_previous`, `compared_with`).
- Plain words: "At power limit 55%" (share of the job's time, normal under full load) instead of "power cap 169 s"; "slowed by heat"; clock-limit reasons explained in one sentence each; the PCIe link glossed ("Gen5 x16 (PCIe 5.0, 16 lanes)"). The limiter on the main view says "power limit" in plain text (normal), heat or a power brake in amber.
- The "Tachyon only reads..." note sits behind a small (i) in the evidence.
- Job report: "What held the clocks back" and "Earlier marked jobs" are two folds.
- History lists marked jobs; the automatic GPU session recorded for the same work is dropped (marked job wins, also in `get_history` and while running). Long names wrap instead of being cut.
- A crash report that says "nothing to do" in its verdict does not repeat it in the card.
- `get_process_vram`: `avg_mb` is the average over the time the process was running (`running_s`), not over the whole window.
- Job reports: the lowest PCIe link counts only readings under load. The idle power-saving link (Gen1/Gen2) just before a job, still read in its first 100 ms samples, made a full-speed job read "Between Gen2 x16 and Gen5 x16" (live test on the RTX 5080, 29 Oct). Reports saved by the 1.2 test builds are revised on load (no degraded event: the lowest under load is the best link).

**Fast sampling (100 ms) for the moments before a stop**
- A small thread of its own reads the SM and memory clocks, power and power limit, temperature, GPU load, the clock-limit (throttle) reasons and the PCIe link every 100 ms (`fast_ms`, 50-1000) into a second fixed-size ring next to the flight recorder: `blackbox/fast_a.bin` / `fast_b.bin` (`TACHYFS1`, 40-byte records with a checksum), 15 minutes by default (`fast_minutes`, 1-60), about 352 KB per session. Written every second, flushed every 5 s, so a power cut loses at most a few seconds. `fast_sampling = false` turns it off.
- `diagnose_crash` adds `last_seconds`: the 30 s before the stop at 100 ms (power against the limit, the biggest one-reading power jump, time at the limit, temperature, SM clock and its biggest drop, each clock-limit reason with how long it was set, the PCIe link, the last reading to the millisecond). Power spikes of +100 W within one reading, power brake, hardware slowdown or a hardware temperature limit become `diagnosis.hints`.
- Job reports keep the 100 ms peaks (peak power, time at the power limit, clock-limited time) without storing the samples.

**PCIe link history**
- The link's generation and width against the best this GPU and slot can do (the lower of the GPU's and the system's maximum generation, and the maximum width).
- NVIDIA cards drop to Gen1/Gen2 when idle; that is counted as normal power saving and written now and then. A link narrower than the slot (after 3 s) or below its generation under load (GPU 50%+ for 5 s) is a *degraded* event, always written, as is the recovery. `history/pcie_link.jsonl`, rotated at 1 MB.
- `get_status.pcie_link`, the new MCP tool `get_pcie_link` (now, the changes of the last days, degraded count), and the bottleneck explainer: an evidence line with the link, and a PCIe verdict says when the link itself runs below its best.

**VRAM per process over time**
- Every 5 s (`proc_vram_secs`, 0 = off) the VRAM each process holds. On Windows NVML answers "N/A" for every process under WDDM, so Tachyon reads the per-process dedicated GPU memory Task Manager shows (`\GPU Process Memory(*)\Dedicated Usage`, pdh.dll from System32, on the adapter whose total matches the NVIDIA card).
- A line goes to `history/proc_vram.jsonl` when a process changes by 64 MB or more, comes or goes, or at least once a minute (largest 16 above 16 MB; rotated at 4 MB).
- The process list now shows each process's VRAM on Windows too; graphics-only processes are listed from 256 MB (they used to be listed with any known VRAM).
- New MCP tool `get_process_vram` (`minutes`, `since`/`until`, `name`, `top`): peak, time-weighted average and last value per process name, first and last seen, a short timeline for the top five, and the reading now.

**Render job markers**
- `tachyon mark start --job NAME` and `tachyon mark end --job NAME` append one line to `markers/markers.jsonl` in the data folder and exit (0 written, 1 could not write, 2 wrong arguments; the line is printed to the console it was started from). Nothing else is touched.
- The running Tachyon follows the file and builds a report per job: duration, average and peak GPU load, memory controller load, VRAM at start / peak / end, average and peak power (100 ms), energy (and cost with `price_per_kwh`), temperature, SM clock range, seconds clock-limited by power and by heat and at the power limit, the clock-limit reasons, the PCIe link at start and end, the lowest under load and degraded events, the processes holding the most VRAM and the most common bottleneck. Jobs started before Tachyon was running are picked up (with `covered_s` saying how much was recorded); a job with no end marker is closed after 48 h; at most 16 open.
- Reports go to `history/marked_jobs.jsonl`; `get_status.marked_jobs` (open, last, notes) and the new read-only MCP tool `get_job_reports` (`n`, `name`). The MCP server stays read-only: it never writes markers.

**Measured on the RTX 5080 (Windows 11, 32 threads), whole Tachyon process, companion mode, 60 s windows**
- Idle, back to back on the same desktop: 1.1.0 1.35% of one core; 1.2 with fast sampling and per-process VRAM off 1.25%; 1.2 with the defaults 1.67% and 1.22% (two runs), and 1.09% in an earlier run. The run-to-run spread (about ±0.3 points, Windows counts CPU time in 15.6 ms ticks) is larger than what 1.2 adds; the fast thread's own time stayed below one tick per minute. Under GPU load: 1.42%. All of it is 0.03-0.05% of the 32-thread CPU. Working set 5-6 MB; private memory 31.5 MB with the defaults vs 28.3 MB for 1.1.0; one extra thread.
- A marked 60 s GPU job (RIG test render, 1080p CUDA + NVENC): 81% average GPU, 2.3 GB VRAM peak, 247 W average, 262 W peak (100 ms), 4.1 Wh, Gen5 x16 throughout.
- After Tachyon was killed, the fast ring's last reading was 0.35 s before the kill (the 1 s recorder's last sample 4.4 s before).

**Build and tests**
- 198 tests on Linux and 202 under wine (169 and 172 in 1.1.0): status scale and percentage rule, same-job comparison, marked jobs over automatic sessions, the lowest PCIe link under load only (the real 29 Oct A2 pattern), the fast ring (wrap-around, damaged records, the thread writing and stopping), the last-seconds summary and hints, the link states (idle dance, degraded width and generation, recovery), the per-process VRAM log and summary, the marker CLI and job reports, the three new MCP tools and `diagnose_crash` with 100 ms data.

## v1.1.0

Two new answers, both worked out from what Tachyon already records. Read-only as before.

**Pre-render check ("will this job fit?")**
- When a render job starts, its peak VRAM and duration are predicted from earlier runs of the same job, the same project or a similar name, at the same resolution (from the job name, the log path or the log's first 16 KB: `4k`, `2160p`, `uhd`, `3840x2160`, `scale=W:H`...).
- Verdict *should fit*, *tight* (over 95% of the card) or *likely out of memory*, against what other apps hold right now, with idle VRAM hogs named ("closing it would leave 15.2 GB free"). A past run that filled the card counts as a lower bound.
- Confidence from the kind of match, the number of runs and how much they agree; below 0.4 there is no verdict, and Hex only speaks up (once per job) at 0.65 or more. Re-checked every 30 s during the job's first 15 minutes.
- Job history lines now also keep the project, resolution, frame count, the job's own peak VRAM, the VRAM in use at the start and the card size (older lines still read).
- Panel line under the bottleneck, a Hex line and announcement, and the MCP tool `preflight` (`name`, `project`, `resolution`, `frames`; no arguments = the running job).

**Crash diagnosis**
- The crash report ranks the probable cause with a percentage: unstable RAM / EXPO, GPU overclock, power / PSU, overheating, driver timeout (TDR), application bug, each with its evidence and one piece of advice, and a confidence (*likely*, *possibly* or *unclear*). Never 100%; Tachyon changes no settings.
- New evidence: the Application log (LiveKernelEvent 141/117 GPU timeout reports, application crashes), bug check codes from Kernel-Power 41 and WER 1001, the GPU clock offsets and power limit vs default (read once a minute and saved next to the flight recorder, so the report knows the settings from before the crash), and the RAM's configured speed vs its standard (JEDEC) speed and the kit's rated EXPO/XMP speed, decoded from the SMBIOS part number (Corsair, G.Skill, Kingston Fury, Crucial / Ballistix, TeamGroup). Running at or below the kit's rating counts as an EXPO/XMP profile in use (a mild factor), only above it as beyond its rating; four DDR5 modules from two kits are flagged.
- In the report view (with *Evidence ›*), Hex's crash announcement, `get_status` (`crash_report.probable_cause`) and the new MCP tool `diagnose_crash` (it also works out older 1.0 reports; without a report, or with `around: "2026-10-22"`, it diagnoses the latest unexpected restart in the Windows Event Log, with the RAM speed and clock offsets as they are now).

**Fixes from a real power loss (26 Oct 2026: the PC went off at idle, Kernel-Power 41 code 0, no dump; the BIOS came back on its defaults)**
- `diagnose_crash` with `around` (or without a crash report) now uses the flight recorder for the minutes before the stop: GPU load, power against the limit, temperatures, the processes on the GPU and whether the PC was idle (`before_the_stop`). It no longer says there are no readings when the recorder has them.
- A power-off with no blue screen while the GPU was idle and at low power now points to *a whole-system power loss or a manual power-off* (new cause, with advice on the socket, PSU switch and what was plugged in or pressed, e.g. the BIOS FlashBack port); *GPU power delivery or the PSU* keeps only a small share. At full load it still points to the PSU.
- `around` is matched against when the PC actually stopped (the recorder's last sample, Windows' last log entry and EventLog 6008's last sign of life), not the restart time, with `tolerance_minutes` (default 90); restart reports logged up to 3 h later are found. The answer gives the stop window (`stopped`), what matched and the other restarts nearby.
- Windows' own times from before the stop are not taken for the stop when the flight recorder ran later: EventLog 6008's "last sign of life" (written only now and then, so 17:31:14 on 26 Oct for a stop at 18:09:04) is marked *stale* in the events list and the report timeline, and `last_log_entry` gets a `last_log_entry_note` saying it is earlier than the recorder's last sample.
- The clock after the restart is checked against the recording: a restart logged an hour after the last sign of life, or a Windows clock correction afterwards (Kernel-General 1), is noted in `clock_note`.
- RAM running below its standard (JEDEC) speed, e.g. 3600 MT/s on 4800 modules, is flagged as *BIOS likely reset to safe defaults (EXPO/XMP off)*, together with a clock that jumped and a slow BIOS start (Windows' "Last BIOS time") when known (`diagnosis.hints`, the report view and the text export).
- `get_blackbox` reads older data: `since`/`until` (local time or Unix ms) or `session: "previous"`/`"current"`, plus a plain `summary` of the last minute, `clock_limits` in words and the `sessions` on disk.
- Process names: 1.0 recorder files wrote the third name over the record checksum, so it read back with garbage (`CrossDeviceResume.ex\x03\xdc,m`). Old files now read back only its 20 real bytes, on whole UTF-8 characters, and a name cut inside `.exe` gets its extension back (`CrossDeviceResume.exe`). New ring files (`TACHYBB2`, 256-byte records) keep names up to 44 bytes; 1.0 files are still read. The flight recorder now takes about 460 KB on disk.
- NVML clock limit 0x400 is named (*reliability policy*), with the other known reasons.

**Build and tests**
- 169 tests on Linux, 172 under wine (132 in 1.0.1): diagnosis rules incl. the real EXPO case, the prediction and its thresholds, resolution parsing (fuzzed), job hints from a projects tree, SMBIOS parsing, the new MCP tools, and a regression on the real 26 Oct recording (`tests/samples/blackbox_2026-10-26_idle_power_loss.bin`, the last 5 minutes; process names only, no paths).
- The fake NVML has an `oc` scenario; `tests/wine.sh` can run on its own prefix/display; `tests/shots_v1_1.sh` makes the 1.1 screenshots.

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
- **Claude Desktop extension:** each release also carries `tachyon-<version>-windows-x64.mcpb` (in `SHA256SUMS.txt` and the attestation); it asks for your `tachyon.toml` so it follows the same *Agent API* switch.
- MCP Registry entry (`packaging/mcp/server.json`, `io.github.formray/tachyon`) and a manual-only *Publish to MCP Registry* workflow; an agent skill in `packaging/skill/tachyon-gpu/`; README setup for Claude, Codex and Grok.

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
