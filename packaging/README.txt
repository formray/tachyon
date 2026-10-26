Tachyon 1.0
===========

The lightest real-time GPU monitor for NVIDIA cards, and the one that explains
itself.

A small window that shows how your NVIDIA graphics card is working, live:
load, temperature, VRAM, power, clocks, fan, video encoder/decoder, PCIe
traffic, what is limiting the clocks, and which programs are using the GPU
(with the program that launched them and a category: Game, Creative app,
Script/Python, Browser, AI/agent, System or Other).

New in 1.0
- Crash report: Tachyon keeps the last 15 minutes of readings in two small files
  on disk (about 170 KB each). If your PC restarts unexpectedly, the next start
  shows what happened just before: the peaks, the program holding the most video
  memory and the matching Windows events (power loss, blue screens, hardware
  errors, NVIDIA driver resets) in plain words. Hex tells you once; right-click
  > "Crash report" opens it any time, "Export flight recorder" saves CSV and JSON.
- "Why is it slow?": one sentence on what is holding your work back (the GPU
  itself, the power limit, heat, the video decoder or encoder, one CPU thread,
  all CPU cores, video memory or PCIe), with how sure Tachyon is. In the panel
  ("Why?") and in Hex's bubble ("Why is it slow?").
- VRAM forecast: a warning when video memory is filling steadily and would run
  out within 15 minutes, and which background apps hold VRAM without using the
  GPU.
- Job history: every finished render, or any stretch of a minute or more with the
  GPU busy, with its duration, load, energy in Wh, peak temperature and VRAM and
  time spent throttled, compared with the previous run of the same job
  ("14% slower than last time"). Right-click > "Job history". Set an energy
  price in Settings > Data to see the cost.
- For AI assistants: "tachyon.exe mcp" lets an MCP client such as Claude Desktop
  or Cursor ask what the GPU is doing. It is off until you turn on "Agent API"
  in Settings > AI agents; "Copy MCP config" there copies the entry to paste
  into your MCP client.
- Settings: right-click > "Settings..." (or the gear in the panel). Change the
  mode, refresh rate, render folders, alert thresholds, crash recorder, job
  history, energy price and the agent API. Changes are saved and applied at
  once; only the crash recorder needs a restart. The previous file is kept as
  tachyon.toml.bak.

Tachyon 5.0 and 5.1 were preview builds before 1.0. What they brought:

Hex, the desktop companion
- Hex is a small glass hexagon with eyes that sits on your desktop, always on
  top. It breathes and blinks now and then, draws a glowing progress line around
  its edge while a render runs, and gets warm (with a little steam) when the
  card is hot. It goes to sleep after 10 quiet minutes.
- Click Hex for a speech bubble: render progress and ETA, GPU load and
  temperature, the program using the most video memory, four key numbers, and
  "Open panel" for the full window. Click again (or wait 8 s; change it in
  Settings) to close it.
- Drag Hex anywhere; its position is remembered.
- Right-click Hex, the island or the panel and pick "Mode" to switch between
  island, companion (Hex) and panel. The choice is remembered (mode = ... in
  tachyon.toml).
- Hex only redraws when something changes (at most a few times per second, and
  not at all while a fullscreen game or video is in front).

The Aurora look
- New look: frosted glass with a soft live colour glow. The glow's colour follows
  the GPU load (blue -> violet) and warms up (amber) as the card gets hot. It
  only moves when the values change, so it costs almost nothing.
- While a render is running, the glow fills the island like a progress bar.
- Big, thin main number; clean lists instead of boxes; coloured category chips
  next to each program.
- Truly borderless: no frame, no grey rectangle, rounded shape with a soft
  shadow. Clicks on the empty area around it go through to the desktop.
- The full window now sizes itself to its content.

Requirements
- An NVIDIA graphics card with a normal NVIDIA driver installed.
  (AMD and Intel graphics are not supported.)
- Windows 10 (1607 or newer) or 11, 64-bit.

How to use
- No installation. Put tachyon.exe in any folder you like and double-click it.
- The island sits at the top of the screen, always on top. Click it to open the
  full panel. In the panel, press the small island button (or double-click the
  top bar) to go back. Drag either one to move it; positions are remembered.
- "Pin" keeps the panel on top. Right-click for a menu (GPU, pin, mode, crash
  report, job history, export, settings, quit).
- Tachyon has no taskbar button; use the island or the right-click menu.
- On first start it creates tachyon.toml next to the exe. That file holds the
  settings (colour thresholds, refresh rate, glass style, process categories)
  and remembers the window and island positions. Most of them are on the
  Settings page; the rest you can edit in the file and restart Tachyon.
- There is no "start with Windows" option yet: to start it automatically, put
  a shortcut to tachyon.exe in the folder that opens with Win+R > shell:startup.
- The crash report data and job history are kept in
  %LOCALAPPDATA%\Tachyon (paste that into the Explorer address bar).
- To remove it, close it and delete tachyon.exe, tachyon.toml (and
  tachyon.toml.bak) and that folder.
  Nothing else is left on your PC.

Windows SmartScreen
- The exe is not code-signed, so Windows may say "Windows protected your PC".
  Click "More info", then "Run anyway". Only do this if you got the file from
  someone you trust.

Privacy and safety
- It only reads. It asks the NVIDIA driver for its sensor values (NVML), the same
  information nvidia-smi shows, and reads the Windows process list (names and
  parent processes) to label the programs using the GPU.
- Program command lines are NOT read unless you set process_cmdline = true.
- It never changes clocks, fans or power limits, never touches other programs,
  and never connects to the internet.
- It writes only its own tachyon.toml (and tachyon.toml.bak) and its folder in %LOCALAPPDATA%\Tachyon
  (recorded readings with program names, never paths or command lines; job
  history). Nothing leaves your PC. Set blackbox = false or history = false to
  turn them off.

Source code and releases: https://github.com/formray/tachyon
Licence: MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE). (c) 2026 Formray.

By Formray - https://formray.io
