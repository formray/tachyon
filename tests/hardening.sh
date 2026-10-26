#!/bin/bash
# Hardening checks under wine (Xvfb + picom, fake NVML; build box only). Assumes wine.sh start has run.
# usage: hardening.sh [first|corrupt|panic|single|all]
export WINEPREFIX=/tmp/tachyon-wine/wp WINEDEBUG=-all DISPLAY=:77
cd "$(dirname "$0")/.."
EXE=${EXE:-target/x86_64-pc-windows-gnu/release/tachyon.exe}
OUT=${OUT:-/tmp/tachyon-work/hardening}; mkdir -p $OUT
R=/tmp/tachyon-work/run_hardening; rm -rf $R; mkdir -p $R; cp $EXE $R/tachyon.exe
DATA_W='C:\tachyon_hardening_data'; DATA=$WINEPREFIX/drive_c/tachyon_hardening_data
fails=0
ok() { echo "  ok   $1"; }
bad() { echo "  FAIL $1"; fails=$((fails+1)); }
stop() { wine taskkill /IM tachyon.exe >/dev/null 2>&1; sleep 2; pkill -f 'tachyon.exe' || true; sleep 1; }
geo() { xdotool search --onlyvisible --name "^$1\$" getwindowgeometry --shell 2>/dev/null | tr '\n' ' '; }
snap() { eval "$(geo Tachyon)"; [ -n "$WIDTH" ] || { echo "  (no window for $1)"; return; }
  import -window root $OUT/root_$1.png; convert $OUT/root_$1.png -crop ${WIDTH}x${HEIGHT}+${X}+${Y} +repage $OUT/$1.png; echo "  shot $1: ${WIDTH}x${HEIGHT}"; }
run() { (cd $R && TACHYON_DATA_DIR="$DATA_W" "$@" timeout ${T:-30} wine tachyon.exe >> $OUT/run.log 2>&1 &) ; }
first() {
  echo "== first start, no settings file"; stop; rm -f $R/tachyon.toml*; rm -rf $DATA
  run env; sleep 9; snap first_start
  [ -f $R/tachyon.toml ] && ok "tachyon.toml written" || bad "no tachyon.toml"
  grep -qi 'users' $R/tachyon.toml && bad "personal path in defaults" || ok "no personal paths in the defaults"
  stop; }
corrupt() {
  echo "== damaged settings file"; stop; printf 'poll_ms = [\n\xff\xfe' > $R/tachyon.toml
  run env; sleep 9; snap corrupt_start
  ls $R | grep -q 'tachyon.toml.corrupt-' && ok "damaged file kept: $(ls $R | grep corrupt-)" || bad "no corrupt copy"
  grep -q 'poll_ms = 1000' $R/tachyon.toml && ok "defaults written in its place" || bad "defaults not written"
  pgrep -f tachyon.exe >/dev/null && ok "still running" || bad "not running"
  stop; }
panic() {
  echo "== panic: crash log and a clean exit"; stop; rm -rf $DATA/logs
  T=20 run env TACHYON_TEST_PANIC=1; sleep 8; snap panic_box
  ls $DATA/logs/panic-*.txt >/dev/null 2>&1 && ok "crash log: $(ls $DATA/logs | tr '\n' ' ')" || bad "no crash log"
  [ -f $DATA/logs/last_panic.txt ] && ok "marker for the next start" || bad "no marker"
  stop
  run env; sleep 9; snap after_panic
  [ -f $DATA/logs/last_panic.txt.seen ] && ok "next start mentioned it once" || bad "marker not taken"
  stop; }
single() {
  echo "== single instance"; stop; printf "mode = 'island'\n" > $R/tachyon.toml
  run env; sleep 8
  n1=$(pgrep -fc 'tachyon.exe$|tachyon.exe ' || true)
  run env; sleep 6
  n2=$(pgrep -f 'wine.*tachyon.exe' | wc -l)
  snap second_start
  c=$(wine tasklist 2>/dev/null | grep -ci tachyon.exe)
  [ "$c" = "1" ] && ok "one tachyon.exe after a second start" || bad "tasklist shows $c tachyon.exe"
  echo "--- a different settings file is a different Tachyon"
  printf "mode = 'island'\n" > $R/other.toml
  run env TACHYON_CONFIG='C:\tachyon_other.toml'; sleep 6
  c=$(wine tasklist 2>/dev/null | grep -ci tachyon.exe)
  [ "$c" = "2" ] && ok "TACHYON_CONFIG gives a separate instance" || bad "tasklist shows $c"
  stop; }
case "${1:-all}" in all) first; corrupt; panic; single;; *) "$1";; esac
echo "fails: $fails"; exit $fails
