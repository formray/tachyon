#!/bin/bash
# 1.1 screenshots (wine + Xvfb + picom, fake NVML v4; build box only).
# Assumes wine.sh start has run with the same WINEPREFIX / TDISPLAY / TT.
# usage: shots_v1_1.sh diagnosis | preflight | all   (OUT= where the PNGs go, default docs/)
export WINEPREFIX=${WINEPREFIX:-/tmp/tachyon-wine/wp} WINEDEBUG=-all DISPLAY=${TDISPLAY:-:77}
cd "$(dirname "$0")/.."
TT=${TT:-/tmp/tachyon-work}
EXE=${EXE:-target/x86_64-pc-windows-gnu/release/tachyon.exe}
OUT=${OUT:-docs}; mkdir -p $OUT
R=$TT/run_v1_1; mkdir -p $R; cp $EXE $R/tachyon.exe
LA=$WINEPREFIX/drive_c/users/$USER/AppData/Local/Tachyon
F=$WINEPREFIX/drive_c/rmfake
SC=$F/scenario.txt
cfg() { # mode render_roots
  printf "render_roots = [%s]\nmode = '%s'\n[gpu]\ntemp_amber_c = 90\ntemp_red_c = 95\n[window]\ncompact = false\nx = 560\ny = 40\nhex_x = 1300\nhex_y = 700\nalways_on_top = false\n" "$2" "$1" > $R/tachyon.toml; }
run() { # secs [env...]
  local s=$1; shift
  (cd $R && env "$@" TACHYON_DEBUG=1 timeout $s wine tachyon.exe >> $TT/shots_v1_1.log 2>&1 &) ; }
geo() { xdotool search --onlyvisible --name "^$1\$" getwindowgeometry --shell 2>/dev/null | tr '\n' ' '; }
# the panel window, without the desktop around it
panel() { eval "$(geo Tachyon)"; [ -z "$WIDTH" ] && { echo "  no panel"; return 1; }
  import -window root $TT/root.png; convert $TT/root.png -crop ${WIDTH}x${HEIGHT}+${X}+${Y} +repage "$1"; echo "  $1 ${WIDTH}x${HEIGHT}"; }
hexshot() { import -window root $TT/root.png; convert $TT/root.png -crop 760x300+840+640 +repage "$1"; echo "  $1"; }
stop() { wine taskkill /IM tachyon.exe >/dev/null 2>&1; sleep 3; pkill -f 'tachyon.exe' || true; sleep 1; }
seed() { TACHYON_DEMO_DIR=$LA cargo test --release --test demo_data -q -- --ignored "$1" >/dev/null 2>&1; }

diagnosis() {
  echo "== crash diagnosis: the real case (nvlddmkm 153 x3, bug check 0x1A, EXPO 6000, +1000 MHz memory)"
  stop; rm -f $SC; rm -rf $LA; mkdir -p $LA; seed write_demo_diagnosis
  cfg panel ""; run 60 TACHYON_VIEW=report; sleep 14; panel $OUT/v1_1_crash_diagnosis.png
  # "Evidence ›" (right edge of the PROBABLE CAUSE caption)
  eval "$(geo Tachyon)"; xdotool mousemove $((X + 456)) $((Y + 336)) click 1; sleep 3
  xdotool mousemove $((X + 300)) $((Y + 150)); sleep 2; panel $OUT/v1_1_crash_evidence.png; stop
  rm -rf $LA; mkdir -p $LA; seed write_demo_diagnosis
  cfg companion ""; run 60; sleep 12; hexshot $OUT/v1_1_hex_crash_diagnosis.png; stop; }

# a render job writing progress.json under the render root, for `secs` seconds
render() {
  local d=$F/projects/demo4k/logs; mkdir -p $d
  ( for i in $(seq 1 $1); do printf '{"job": "demo4k try 3", "frame": %d, "total": 1420, "fps": 0.8}' $((40 + i)) > $d/progress.json.tmp
      mv $d/progress.json.tmp $d/progress.json; sleep 1; done ) &
  RENDER_PID=$!
}

preflight() {
  echo "== pre-render check: three 4K runs needed ~15.2 GB; ~14 GB free now with chrome.exe idle on 1.2 GB"
  stop; rm -f $SC; cp $F/procs.txt $TT/procs.txt.bak
  pid() { tr -d '\r\n' < $F/pids/$1.pid; }
  printf "%s\n" "$(pid ffmpeg) 9000 C 92 0 0" "$(pid chrome) 1230 G 0 0 0" "$(pid dwm) -1 G 0 0 0" > $F/procs.txt
  rm -rf $LA; mkdir -p $LA; seed write_demo_preflight_history
  cfg panel "'C:\\\\rmfake\\\\projects'"; render 140; run 140; sleep 100; panel $OUT/v1_1_preflight_panel.png; stop
  rm -rf $LA; mkdir -p $LA; seed write_demo_preflight_history
  cfg companion "'C:\\\\rmfake\\\\projects'"; render 60; run 60; sleep 14; hexshot $OUT/v1_1_hex_preflight.png; stop
  kill $RENDER_PID 2>/dev/null; sleep 2; cp $TT/procs.txt.bak $F/procs.txt; rm -rf $F/projects; }

case "$1" in
diagnosis|preflight) $1 ;;
all) diagnosis; preflight ;;
esac
