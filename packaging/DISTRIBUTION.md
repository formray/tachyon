# Distribution

Where Tachyon can be published, what is ready in this folder, and what still needs a
person. Nothing here has been submitted.

## Ready in this folder

| File | For | Before submitting |
|---|---|---|
| `winget/manifests/f/Formray/Tachyon/1.0.1/` | winget (`winget install Formray.Tachyon`): zip with a nested portable exe, command alias `tachyon` | run `update-hashes.sh 1.0.1` after the release, then `winget validate` and `winget install --manifest` on a Windows PC |
| `scoop/tachyon.json` | Scoop: a bucket of our own, or a pull request to the `extras` bucket | `update-hashes.sh 1.0.1`; `checkver` and `autoupdate` read later releases and their `SHA256SUMS.txt` |
| `mcpb/` | Claude Desktop extension, packed by `release.yml` into `tachyon-<v>-windows-x64.mcpb` | nothing: built, hashed and attested with each release |
| `mcp/server.json` | official MCP Registry, `io.github.formray/tachyon` (`registryType: mcpb`) | run *Actions → Publish to MCP Registry* by hand with the tag, first with *dry run*; it fills version, URL and `fileSha256` from the release and logs in with GitHub OIDC (needs an Owner of the formray org to allow it). Never runs by itself |
| `skill/tachyon-gpu/SKILL.md` | Agent Skill for Claude, Codex, Grok | ships in the repo; a plugin repo can reuse it |
| `update-hashes.sh` | fills both hashes from the release's `SHA256SUMS.txt` | — |

With winget or Scoop the exe's folder is replaced on every update, so Tachyon notices
those folders and keeps `tachyon.toml` in `%LOCALAPPDATA%\Tachyon` from the start
(`config::managed_install`). *Start with Windows* points at the exe that turned it
on; after a Scoop update turn it off and on once (or start Tachyon from the Scoop
shortcut, which uses the `current` folder).

### Steps for winget

1. Publish the GitHub release `v1.0.1` (the zip URL in the manifest must exist).
2. `packaging/update-hashes.sh 1.0.1`, commit.
3. On Windows: `winget validate --manifest packaging\winget\manifests\f\Formray\Tachyon\1.0.1`
   and `winget install --manifest …` (needs `winget settings --enable LocalManifestFiles`
   once, as admin; use a test PC or VM).
4. Fork `microsoft/winget-pkgs`, copy the folder to `manifests/f/Formray/Tachyon/1.0.1/`,
   open a pull request (or `wingetcreate submit`). The bot runs its own checks,
   including a SmartScreen/Defender scan; unsigned portable apps are accepted.
5. Later versions: `wingetcreate update Formray.Tachyon --version X --urls <zip url> --submit`.

### Steps for Scoop

Either a bucket of our own (a repo `formray/scoop-bucket` with `bucket/tachyon.json`;
users run `scoop bucket add formray https://github.com/formray/scoop-bucket` then
`scoop install tachyon`), or a pull request to `ScoopInstaller/Extras`, which takes
GUI apps with some users and a stable download URL.

## SmartScreen and code signing

`tachyon.exe` is not signed. On first run Windows SmartScreen shows *Windows protected
your PC* (*More info → Run anyway*), and some company PCs block unsigned exes outright.
Signing does not remove the warning on day one either: SmartScreen trusts a signing
identity once it has built up reputation, so it matters to sign every release with the
same identity, starting early. Options, cheapest first:

| Option | Cost | Who | Notes |
|---|---|---|---|
| **SignPath Foundation** (signpath.org) | free for OSS | open-source projects with an OSI licence (MIT OR Apache-2.0 is fine), already released, actively maintained | the certificate names *SignPath Foundation* as publisher, not Formray; builds must come from CI (GitHub Actions is supported), MFA for everyone, a published signing policy and a manual approval per release. Apply at signpath.org/apply |
| **Azure Artifact Signing** (formerly Trusted Signing) | about $9.99/month (5,000 signatures) | organisations in the EU, UK, US, Canada and others; individuals only in the US and Canada | publisher shows as *Formray* after organisation identity validation (a few working days; needs a paid Azure subscription). GitHub Action `azure/artifact-signing-action` signs in the release workflow; no hardware token |
| OV certificate from a CA (Sectigo, DigiCert, Certum…) | €150–300/year | anyone | key on a hardware token or a cloud HSM, which makes CI signing harder |
| EV certificate | €400+/year | companies | since 2024 no faster SmartScreen trust than OV; not worth it for this |

Suggested path: Azure Artifact Signing under Formray (the publisher name users see is
ours), signing `tachyon.exe` in `release.yml` before it is zipped, hashed and attested.
SignPath Foundation is the free alternative if a third-party publisher name is fine.
Either way, keep `SHA256SUMS.txt` and the provenance attestation: they prove which
commit a download came from, which a signature alone does not.

## Other places

- **GitHub Releases** (now): exe, zip, `SHA256SUMS.txt`, provenance attestation.
- **winget** and **Scoop**: above.
- **Chocolatey**: possible (a `.nuspec` with the zip URL and checksum); moderation takes
  days and flags unsigned binaries for manual review. Not prepared yet.
- **Microsoft Store**: would need an MSIX package (and then the Store signs it, which
  removes the SmartScreen problem). Tachyon would have to keep its settings in
  `%LOCALAPPDATA%` only, which the managed-install path already does. Not prepared.
