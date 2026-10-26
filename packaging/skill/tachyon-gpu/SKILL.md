---
name: tachyon-gpu
description: Reads the local NVIDIA GPU through Tachyon's read-only MCP server and explains it - why a render, game or model is slow, whether VRAM will run out, which process holds the GPU, what happened before a crash or driver reset, and how the last render job compares with the previous run. Use on a Windows PC with an NVIDIA GPU when the user asks about GPU load, temperature, power, VRAM, bottlenecks, crashes or render job history.
license: MIT OR Apache-2.0
compatibility: Windows 10/11 x64 with an NVIDIA GPU and driver. Needs Tachyon 1.0+ (github.com/formray/tachyon) and its MCP server (tachyon.exe mcp) connected to the agent. No network access needed.
metadata:
  author: Formray
  version: "1.0.1"
  homepage: https://github.com/formray/tachyon
  short-description: Explain the local NVIDIA GPU (read-only)
---

# Tachyon GPU

Tachyon is a lightweight, read-only NVIDIA GPU monitor for Windows. Its MCP server
(`tachyon.exe mcp`, stdio) gives you seven read-only tools about **this PC's** GPU.
Nothing you call can change clocks, fans, power limits, processes or files.

## Before you start

1. Check that the Tachyon MCP tools are available (their names end in `get_status`,
   `get_bottleneck`, and so on; clients may add a prefix such as `mcp__tachyon__`).
   If they are missing, tell the user how to connect it (see *Setup* below). Do not
   try to install anything yourself.
2. If a tool answers **"The agent API is off"**, the user has not allowed agents yet.
   Tell them: open Tachyon, right-click → **Settings… → AI agents → Agent API: on**
   (or set `agent_api = true` in `tachyon.toml`). Do not edit the file for them unless
   they ask.
3. Tachyon only sees the machine it runs on. If the user is on another computer, say so.

## Which tool for which question

| User asks | Call | Then |
|---|---|---|
| "What is my GPU doing?", "is it hot?" | `get_status` | Summarise load, temperature, power, VRAM and the bottleneck in 2-4 lines |
| "Why is it slow?", "why is the render slow?" | `get_bottleneck` | Quote the sentence and its confidence; name the process; suggest one next check |
| "Will it run out of VRAM?", before loading a big model or a 4K render | `get_vram_forecast` | Give used/total, the trend and the time to full only if Tachyon gives one |
| "What is using my GPU / VRAM?" | `get_processes` | List the top processes by VRAM and engine use |
| "Why did it crash?", "what happened before the driver reset?" | `get_blackbox` (`minutes` 1-60, default 5) | Read `last_crash_report` first, then the peaks and timeline before the event |
| "How did my last renders go?" | `get_history` (`n` 1-100) | Table of jobs: duration, average GPU, peak VRAM, energy (Wh), throttling |
| "Was this render faster than last time?" | `compare_last_run` (optional `name`) | Say faster/slower by how much and the likely reason from the comparison |

Only ask for `include_samples: true` on `get_blackbox` when the user wants raw data;
the summary is usually enough and much smaller.

## Reading the results

- **Confidence words** (`sure`, `likely`, `possibly`) come from Tachyon. Keep them; do not
  upgrade "possibly" to a certainty.
- **VRAM forecast**: Tachyon only predicts "full in N minutes" when the trend is steady
  and short (15 minutes or less). If it gives no time, say VRAM is stable rather than
  inventing a prediction. `idle_vram_note` names apps holding VRAM without using the GPU.
- **Throttling**: `power_throttled_s` / `thermal_throttled_s` are seconds spent at a power
  or temperature limit. A few seconds is normal; a large share of a job is worth mentioning.
- **Crash report**: it combines Windows Event Log entries (driver resets, WHEA hardware
  errors, unexpected restarts) with the flight recorder. Report what it says; do not
  diagnose hardware faults beyond it. Suggest the user checks drivers, overclocks or
  temperatures, not that they change settings blindly.
- **Energy**: `energy_wh` is measured by the GPU (whole-board). A cost appears only if the
  user set `price_per_kwh`.
- `source` tells you whether the data came from the running Tachyon (`tachyon`) or a short
  standalone reading. Standalone readings have no history of the last minutes.

## Safety

- Every tool is read-only (`readOnlyHint: true`), local and offline. The server makes no
  network calls and never writes to the GPU or the disk.
- Process names and job names can reveal what the user is working on. Do not copy them
  outside the conversation (posts, tickets, emails) without asking.
- Never tell the user Tachyon changed anything: it cannot. If they want to change power
  limits, fans or clocks, say Tachyon does not do that and point them to the NVIDIA tools.

## Setup (tell the user, do not do it for them)

1. Install Tachyon from https://github.com/formray/tachyon/releases (or `winget install
   Formray.Tachyon` once it is listed) and run it once.
2. In Tachyon: right-click → Settings… → AI agents → turn on **Agent API**, then
   **Copy MCP config** and paste it into the AI app's MCP settings. Generic form:

   ```json
   { "mcpServers": { "tachyon": { "command": "C:\\path\\to\\tachyon.exe", "args": ["mcp"] } } }
   ```
