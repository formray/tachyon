Tachyon 5.1
===========

The lightest real-time GPU monitor. Aurora glass island for NVIDIA cards.

A small window that shows how your NVIDIA graphics card is working, live:
load, temperature, VRAM, power, clocks, fan, video encoder/decoder, PCIe
traffic, what is limiting the clocks, and which programs are using the GPU
(with the program that launched them and a category: Game, Creative app,
Script/Python, Browser, AI/agent, System or Other).

New in 5.1: Hex, the desktop companion
- Hex is a small glass hexagon with eyes that sits on your desktop, always on
  top. It breathes and blinks now and then, draws a glowing progress line around
  its edge while a render runs, and gets warm (with a little steam) when the
  card is hot. It goes to sleep after 10 quiet minutes.
- Click Hex for a speech bubble: render progress and ETA, GPU load and
  temperature, the program using the most video memory, four key numbers, and
  "Open panel" for the full window. Click again (or wait 8 s) to close it.
- Drag Hex anywhere; its position is remembered.
- Right-click Hex, the island or the panel and pick "Mode" to switch between
  island, companion (Hex) and panel. The choice is remembered (mode = ... in
  tachyon.toml).
- Hex only redraws when something changes (at most a few times per second, and
  not at all while a fullscreen game or video is in front).

New in 5.0 "Aurora"
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
- "Pin" keeps the panel on top. Right-click for a menu (GPU, pin, island, quit).
- Tachyon has no taskbar button; use the island or the right-click menu.
- On first start it creates tachyon.toml next to the exe. That file holds the
  settings (colour thresholds, refresh rate, glass style, process categories)
  and remembers the window and island positions.
- To remove it, close it and delete tachyon.exe and tachyon.toml. Nothing else is
  left on your PC.

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
- The only file it writes is its own tachyon.toml.

Source code and releases: https://github.com/formray/tachyon
Licence: MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE). (c) 2026 Formray.

By Formray - https://formray.io
