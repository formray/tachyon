#!/bin/bash
# 1.2 layout screenshots (wine + Xvfb + picom, fake NVML v4; build box only).
# Seeds the data folder with a copy of real files (SEED=dir with blackbox/ and history/), opens
# each view at a display scale (DPI=96|120|144) and crops the panel.
# usage: shots_v1_2.sh <name-prefix> [views...]   (EXE=, OUT=, SEED=, DPI=)
export WINEPREFIX=${WINEPREFIX:-/tmp/tachyon-wine/wp} WINEDEBUG=-all DISPLAY=${TDISPLAY:-:77}
cd "$(dirname "$0")/.."
TT=${TT:-/tmp/tachyon-work}
EXE=${EXE:-target/x86_64-pc-windows-gnu/release/tachyon.exe}
OUT=${OUT:-/tmp/shots_v1_2}; mkdir -p $OUT
DPI=${DPI:-96}
R=$TT/run_v1_2; mkdir -p $R; cp $EXE $R/tachyon.exe
LA=$WINEPREFIX/drive_c/users/$USER/AppData/Local/Tachyon
pre=$1; shift
views=${*:-main report evidence job}
printf "mode = 'panel'\n[window]\ncompact = false\nx = 560\ny = 20\nalways_on_top = false\n" > $R/tachyon.toml
wine reg add 'HKCU\Control Panel\Desktop' /v LogPixels /t REG_DWORD /d $DPI /f >/dev/null 2>&1
wine reg add 'HKCU\Software\Wine\Fonts' /v LogPixels /t REG_DWORD /d $DPI /f >/dev/null 2>&1
# the DPI is read when wine starts: restart it (and the fake GPU processes) when it changed
if [ "$(cat $TT/dpi_v1_2 2>/dev/null)" != "$DPI" ]; then
  wineserver -k; sleep 2; (TDISPLAY=${DISPLAY} bash tests/wine.sh start >/dev/null 2>&1 &); sleep 25; echo $DPI > $TT/dpi_v1_2
fi
geo() { xdotool search --onlyvisible --name "^$1\$" getwindowgeometry --shell 2>/dev/null | tr '\n' ' '; }
panel() { eval "$(geo Tachyon)"; [ -z "$WIDTH" ] && { echo "  no panel"; return 1; }
  import -window root $TT/root.png; convert $TT/root.png -crop ${WIDTH}x${HEIGHT}+${X}+${Y} +repage "$1"; echo "  $1 ${WIDTH}x${HEIGHT}"; }
stop() { wine taskkill /IM tachyon.exe >/dev/null 2>&1; sleep 2; pkill -f 'run_v1_2/tachyon.exe' || true; sleep 1; }
seed() { rm -rf $LA; mkdir -p $LA; cp -r $SEED/. $LA/; }
for v in $views; do
  stop; seed
  case $v in
    main) env=""; ;;
    report|evidence|others) env="TACHYON_VIEW=report" ;;
    job|jobmore) env="TACHYON_VIEW=job" ;;
    history) env="TACHYON_VIEW=history" ;;
  esac
  (cd $R && env $env timeout 40 wine tachyon.exe >> $TT/shots_v1_2.log 2>&1 &)
  sleep 9
  eval "$(geo Tachyon)"
  # click at a point given in 96-dpi window pixels, scaled to this DPI
  click() { xdotool mousemove $((X + $(awk "BEGIN{printf \"%d\", $1*$DPI/96}"))) $((Y + $(awk "BEGIN{printf \"%d\", $2*$DPI/96}"))) click 1; sleep 2; xdotool mousemove $((X + 5)) $((Y + 5)); sleep 1; }
  case $v in
    evidence) [ -n "$EVY" ] && click 200 $EVY ;;
    others) [ -n "$OTY" ] && click 200 $OTY ;;
    jobmore) [ -n "$JMY" ] && click 200 $JMY ;;
  esac
  panel $OUT/${pre}_${v}_dpi${DPI}.png
done
stop
