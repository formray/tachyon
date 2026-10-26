# Security

## Reporting a problem

Please report security problems privately through GitHub: **Security → Report a
vulnerability** on [formray/tachyon](https://github.com/formray/tachyon/security/advisories/new).
Do not open a public issue for them. Include the Tachyon version, Windows build and
the steps to reproduce; a crash log from `%LOCALAPPDATA%\Tachyon\logs\` helps.
You should get an answer within a week. Fixes go into the next patch release and
are credited in the changelog unless you prefer otherwise.

Supported: the latest release. Older releases and the 5.x previews get no fixes.

## What Tachyon does and does not do

Tachyon is a read-only monitor. The design keeps what it can touch small:

- **No network.** There is no network code and the exe imports no network
  library (no Winsock, WinHTTP or WinINet): no telemetry, no update check, no
  crash upload.
- **No process launching.** Tachyon never starts other programs or shell
  commands. It reads other processes' names (and, only with
  `process_cmdline = true`, their command lines) but never opens them for
  writing, never injects anything and never changes clocks, fans or power limits.
- **Writes in two places only:** its `tachyon.toml` (with `.bak` and
  `.corrupt-…` copies beside it) and `%LOCALAPPDATA%\Tachyon` (flight
  recorder, history, exports, crash logs). The one exception is the
  *Start with Windows* shortcut in the user's Startup folder, written only
  when that switch is turned on and removed when it is turned off. No
  registry writes, no admin rights.
- **The agent API is off by default and read-only.** `tachyon.exe mcp` speaks
  MCP over stdin/stdout to the client that started it; it listens on no port.
  Its seven tools only read. With `agent_api = false` it answers that the API
  is off. The running app shares its status through a named shared-memory
  section whose security descriptor allows only the current user (and
  SYSTEM), with a medium integrity label that also keeps sandboxed
  (low-integrity) processes from reading or writing it;
  `tachyon mcp` refuses a section that another account created.
- **One instance per settings file**, through a mutex with the same per-user
  security descriptor.
- **DLLs:** NVML is loaded from `System32` or the NVIDIA NVSMI folder by full
  path, and every other library loaded at run time comes from `System32` only
  (`SetDefaultDllDirectories`), so a DLL dropped next to `tachyon.exe` is not
  picked up at run time. Keep `tachyon.exe` in a folder only you can write to,
  as with any program.

## Untrusted input

Everything Tachyon reads from disk is treated as hostile and size-capped:
`tachyon.toml` (1 MB; unreadable files are kept aside and replaced by
defaults), render logs (the last 8 MB), `history\jobs.jsonl` (damaged lines are
skipped), the flight recorder (fixed-size checksummed records, 8 MB cap),
`last_report.json` (4 MB) and the Windows event XML. MCP requests are capped at
1 MB per line and 64 per batch; malformed JSON and invalid UTF-8 get JSON-RPC
errors and the server keeps going. `cargo test` runs randomised and mutated
inputs through each of these parsers (`tests/robustness_test.rs`; set
`TACHYON_FUZZ_ROUNDS` for more rounds) and checks that nothing panics or hangs.
If Tachyon does panic, it writes a short log to `%LOCALAPPDATA%\Tachyon\logs\`
and exits; the next start says so.

## Builds and supply chain

- Releases are built by GitHub Actions from the tagged commit
  (`.github/workflows/release.yml`), with `--locked` dependencies.
- Every release has `SHA256SUMS.txt` and a signed build-provenance attestation
  for `tachyon.exe` and the zip. Check a download with
  `gh attestation verify tachyon.exe --repo formray/tachyon`, or compare
  `Get-FileHash tachyon.exe` with `SHA256SUMS.txt`.
- CI runs `cargo deny` (RustSec advisories, licences, crate sources) and
  `cargo audit` on every push and pull request. All actions are pinned by
  commit SHA and updated through Dependabot.
- Eight direct dependencies, all MIT, Apache-2.0 or BSD-3-Clause, all from
  crates.io.
- The exe is not code-signed yet, so SmartScreen may warn on first start.
