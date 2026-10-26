#!/bin/bash
# Fill the winget and Scoop hashes from a published release's SHA256SUMS.txt.
# usage: packaging/update-hashes.sh 1.0.1 [SHA256SUMS.txt]   (downloads it from the release when no file is given)
set -euo pipefail
v=${1:?version, e.g. 1.0.1}
cd "$(dirname "$0")"
sums=${2:-}
if [ -z "$sums" ]; then
  sums=$(mktemp)
  curl -fsSL "https://github.com/formray/tachyon/releases/download/v$v/SHA256SUMS.txt" -o "$sums"
fi
h=$(awk -v z="Tachyon_v$v.zip" '$2==z || $2=="*"z {print $1}' "$sums")
[[ "$h" =~ ^[0-9a-f]{64}$ ]] || { echo "no hash for Tachyon_v$v.zip in $sums"; exit 1; }
W=winget/manifests/f/Formray/Tachyon/$v/Formray.Tachyon.installer.yaml
sed -i "s/InstallerSha256: .*/InstallerSha256: ${h^^}/" "$W"
sed -i "s/\"hash\": \"[0-9a-fA-F]\{64\}\"/\"hash\": \"$h\"/" scoop/tachyon.json
echo "Tachyon_v$v.zip $h"
grep -n "InstallerSha256" "$W"; grep -n '"hash": "' scoop/tachyon.json
