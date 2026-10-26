#!/bin/bash
# Settings page screenshots and click-through checks (wine + Xvfb + picom, fake NVML v4; build box only).
# Assumes wine.sh start has run. usage: shots_settings.sh shots | clicks
export WINEPREFIX=/tmp/tachyon-wine/wp WINEDEBUG=-all DISPLAY=:77
cd "$(dirname "$0")/.."
EXE=${EXE:-target/x86_64-pc-windows-gnu/release/tachyon.exe}
OUT=${OUT:-/tmp/tachyon-work/shots_settings}; mkdir -p $OUT
R=/tmp/tachyon-work/run_settings; mkdir -p $R; cp $EXE $R/tachyon.exe
mkdir -p $WINEPREFIX/drive_c/work/projects $WINEPREFIX/drive_c/work/renders
cfg() {
  cat > $R/tachyon.toml <<TOML
# my own comment, kept by the Settings page
render_roots = ['C:\\work\\projects', 'C:\\work\\renders']
mode = 'panel'
price_per_kwh = 0.30
future_key = "kept"
[stage_names]
r = "stage A render"
[gpu]
util_band_low = 77
util_band_high = 82
temp_amber_c = 80
temp_red_c = 83
vram_amber_mb = 14500
vram_red_mb = 15600
[window]
compact = false
x = 560
y = 80
always_on_top = false
TOML
}
geo() { xdotool search --onlyvisible --name "^$1\$" getwindowgeometry --shell 2>/dev/null | tr '\n' ' '; }
stop() { wine taskkill /IM tachyon.exe >/dev/null 2>&1; sleep 3; pkill -f 'tachyon.exe' || true; sleep 1; }
snap() { # name: crop the panel window from the root
  eval "$(geo Tachyon)"; import -window root $OUT/root_$1.png
  convert $OUT/root_$1.png -crop ${WIDTH}x${HEIGHT}+${X}+${Y} +repage $OUT/$1.png; echo "  $1: ${WIDTH}x${HEIGHT}+${X}+${Y}"; }
shots() {
  for tab in general projects alerts data ai; do
    stop; cfg
    (cd $R && TACHYON_VIEW=settings TACHYON_SETTINGS_TAB=$tab TACHYON_DEBUG=1 timeout 40 wine tachyon.exe >> $OUT/run.log 2>&1 &)
    sleep 9; snap settings_$tab
  done
  stop; }
# click a point given in panel logical px (scale 1 under Xvfb; the panel has a 30 px shadow pad)
click() { eval "$(geo Tachyon)"; xdotool mousemove $((X + 30 + $1)) $((Y + 30 + $2)) click 1; sleep 1.2; }
clicks() {
  stop; cfg; cp $R/tachyon.toml $OUT/before.toml
  (cd $R && TACHYON_VIEW=settings TACHYON_SETTINGS_TAB=ai TACHYON_DEBUG=1 timeout 60 wine tachyon.exe >> $OUT/run.log 2>&1 &)
  sleep 9; snap clicks_ai_before
  click ${AGENT_X:-436} ${AGENT_Y:-297}   # Agent API toggle
  sleep 2; snap clicks_ai_after
  ls -l $R/; echo "--- tachyon.toml after"; cat $R/tachyon.toml; echo "--- diff vs before"; diff $OUT/before.toml $R/tachyon.toml
  sleep 2; stop; }
"$@"
