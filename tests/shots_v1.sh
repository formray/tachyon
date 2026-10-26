#!/bin/bash
# 1.0 screenshots and interaction checks (wine + Xvfb + picom, fake NVML v4; build box only).
# Assumes wine.sh start has run. usage: shots_v1.sh crash | bottleneck | forecast | history | all
export WINEPREFIX=/tmp/tachyon-wine/wp WINEDEBUG=-all DISPLAY=:77
cd "$(dirname "$0")/.."
EXE=${EXE:-target/x86_64-pc-windows-gnu/release/tachyon.exe}
OUT=${OUT:-/tmp/tachyon-work/shots_v1}; mkdir -p $OUT
R=/tmp/tachyon-work/run_v1; mkdir -p $R; cp $EXE $R/tachyon.exe
LA=$WINEPREFIX/drive_c/users/$USER/AppData/Local/Tachyon
SC=$WINEPREFIX/drive_c/rmfake/scenario.txt
cfg() { # mode extra
  printf "render_roots = []\nmode = '%s'\n%s\n[gpu]\ntemp_amber_c = 90\ntemp_red_c = 95\n[window]\ncompact = false\nx = 560\ny = 40\nhex_x = 1300\nhex_y = 700\nalways_on_top = false\n" "$1" "$2" > $R/tachyon.toml; }
run() { # secs [env...]
  local s=$1; shift
  (cd $R && env "$@" TACHYON_DEBUG=1 timeout $s wine tachyon.exe >> $OUT/run.log 2>&1 &) ; }
geo() { xdotool search --onlyvisible --name "^$1\$" getwindowgeometry --shell 2>/dev/null | tr '\n' ' '; }
vis() { for n in "Tachyon" "Tachyon Hex" "Tachyon Hex bubble"; do printf "  %-20s %s\n" "$n:" "$(geo "$n")"; done; }
# click the bubble's right (or left) link: shape = window minus the 44 px shadow padding
blink() { eval "$(geo 'Tachyon Hex bubble')"; [ -z "$WIDTH" ] && { echo "  no bubble"; return 1; }
  if [ "$1" = left ]; then x=$((X + 44 + 18 + 30)); else x=$((X + WIDTH - 44 - 18 - 30)); fi
  xdotool mousemove $x $((Y + HEIGHT - 44 - 22)) click 1; }
crop() { convert "$1" -crop "$2" +repage "$3"; }
hexclick() { xdotool mousemove 1380 782 click 1; }
# close cleanly (WM_CLOSE), so the next start does not report an unexpected stop
stop() { wine taskkill /IM tachyon.exe >/dev/null 2>&1; sleep 3; pkill -f 'tachyon.exe' || true; sleep 1; }

crash() {
  echo "== crash report: seeded unclean session ending before this machine booted"
  stop; rm -f $SC; rm -rf $LA; mkdir -p $LA
  TACHYON_DEMO_DIR=$LA cargo test --release --test demo_data -q -- --ignored >/dev/null 2>&1
  cfg companion ""; run 90
  sleep 10; vis; import -window root $OUT/crash_announce_root.png
  crop $OUT/crash_announce_root.png 760x300+840+640 $OUT/hex_crash_announcement.png
  blink right; sleep 3; echo "  after Open report:"; vis
  import -window root $OUT/crash_report_root.png
  python3 - "$LA/blackbox/last_report.json" <<'PY'
import json,sys; r=json.load(open(sys.argv[1])); print("  report:", r["cause"], "|", r["headline"], "|", r["events_note"], "| announced", r["announced"], "| samples", r["samples"])
PY
  sleep 26; stop; }

bottleneck() {
  echo "== bottleneck: decode-bound scenario, Hex bubble and 'Why is it slow?'"
  stop; echo decode > $SC; cfg companion ""; run 90
  sleep 22; hexclick; sleep 2; vis; import -window root $OUT/bottleneck_root.png
  crop $OUT/bottleneck_root.png 760x300+840+640 $OUT/hex_bottleneck.png
  blink left; sleep 2; import -window root $OUT/why_root.png
  crop $OUT/why_root.png 760x300+840+640 $OUT/hex_why.png
  sleep 18; stop; rm -f $SC; }

forecast() {
  echo "== VRAM forecast: steady 25 MB/s ramp, panel and Hex"
  stop; rm -f $LA/blackbox/last_report.json; echo ramp > $SC; cfg panel ""; run 120
  sleep 62; import -window root $OUT/forecast_root.png; sleep 10; stop
  echo ramp > $SC; cfg companion ""; run 120; sleep 52; hexclick; sleep 2; import -window root $OUT/forecast_hex_root.png
  crop $OUT/forecast_hex_root.png 760x300+840+640 $OUT/hex_forecast.png; sleep 9; stop; rm -f $SC; }

history() {
  echo "== history view: a real GPU session recorded under wine (busy 2 min, clean close), then the view"
  stop; rm -rf $LA/history /tmp/tachyon-work/seed; mkdir -p $LA
  TACHYON_DEMO_DIR=/tmp/tachyon-work/seed cargo test --release --test demo_data -q -- --ignored >/dev/null 2>&1
  cp -r /tmp/tachyon-work/seed/history $LA/
  echo busy > $SC; cfg panel ""; run 300; sleep 130; stop; rm -f $SC
  tail -1 $LA/history/jobs.jsonl | cut -c1-200
  cfg panel ""; run 60 TACHYON_VIEW=history; sleep 14; import -window root $OUT/history_root.png; sleep 8; stop; }

case "$1" in
crash|bottleneck|forecast|history) $1 ;;
all) crash; bottleneck; forecast; history ;;
esac
