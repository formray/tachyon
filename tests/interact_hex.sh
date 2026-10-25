#!/bin/bash
# Companion interaction test (wine + Xvfb + picom; build box only): click Hex -> bubble,
# "Open panel" -> main window shown, drag Hex -> position saved.
export WINEPREFIX=/tmp/tachyon-wine/wp WINEDEBUG=-all DISPLAY=:77
R=/tmp/tachyon-work/run5; mkdir -p $R; pkill -f tachyon.exe; sleep 1
cp $(dirname "$0")/../target/x86_64-pc-windows-gnu/release/tachyon.exe $R/
printf "render_roots = []\nmode = 'companion'\n[gpu]\ntemp_amber_c = 90\ntemp_red_c = 95\n[window]\ncompact = false\nx = 560\ny = 40\nhex_x = 1300\nhex_y = 700\n" > $R/tachyon.toml
(cd $R && timeout 40 wine tachyon.exe >/dev/null 2>&1 &)
sleep 10
vis() { for n in "Tachyon" "Tachyon Hex" "Tachyon Hex bubble"; do printf "%-20s %s\n" "$n:" "$(xdotool search --onlyvisible --name "^$n\$" getwindowgeometry 2>/dev/null | tr -s ' \n' ' ')"; done; }
echo "== start"; vis
xdotool mousemove 1380 782 click 1; sleep 2; echo "== after hex click"; vis
xdotool mousemove 1240 864 click 1; sleep 2; echo "== after Open panel"; vis
xdotool mousemove 1380 782 mousedown 1; sleep 0.2; xdotool mousemove 1370 770; sleep 0.2; xdotool mousemove 1000 500; sleep 0.3; xdotool mouseup 1; sleep 2
echo "== after drag"; vis
grep -E "^mode|hex_" $R/tachyon.toml
xdotool search --name "^Tachyon Hex$" windowclose 2>/dev/null; sleep 3; echo "== after quit via WM_CLOSE"; grep -E "^mode|hex_" $R/tachyon.toml
pkill -f tachyon.exe; true
