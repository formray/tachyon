# Changelog

## v5.1.0

Tachyon 5.1 brings **Hex**, a desktop companion, alongside the new **Aurora** look.

- **Hex companion:** a small glass hexagon with eyes that sits on your desktop. It breathes and blinks, draws a glowing progress line round its edge during a render, warms up (with a little steam) when the card is hot and dozes off after ten quiet minutes. Click it for a bubble with render progress, load, temperature, the biggest VRAM user and four key numbers, or open the full panel from there.
- **Modes:** island, companion or panel, switched from the right-click menu and remembered (`mode = ...` in `tachyon.toml`).
- **Aurora glass:** frosted glass with a live colour field that follows GPU load and temperature; truly borderless windows with per-pixel alpha, a soft shadow and click-through around the shapes. The island fills like a progress bar during a render; the panel sizes itself to its content.
- **GPU process attribution** with category chips, launcher chains and per-process SM/ENC/DEC and VRAM.
- **Still light:** no constant animation timer; about 0.3 % of one core idle under wine (real Windows figures to follow). One portable exe of about 1.4 MB.
- `glass = true/false` from 4.x settings files still loads.

Known limits: not yet run on real Windows; no real backdrop blur (`glass = "blur"` is experimental); NVIDIA only. See the README for the full list.
