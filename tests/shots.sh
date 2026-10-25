#!/bin/bash
# Screenshot scenarios (wine + Xvfb + picom compositor over a busy wallpaper; build box only).
# Assumes wine.sh start has run. The X root is captured, so what you see is the real layered
# window blended by the compositor: no rectangle means the transparent pixels really are transparent.
export WINEPREFIX=/tmp/tachyon-wine/wp WINEDEBUG=-all DISPLAY=:77
EXE=${EXE:-$(dirname "$0")/../target/x86_64-pc-windows-gnu/release/tachyon.exe}
OUT=${OUT:-/tmp/tachyon-work/shots5}; mkdir -p $OUT
R=/tmp/tachyon-work/run5; mkdir -p $R; cp $EXE $R/tachyon.exe
PN=${PN:-demo4k}
PROJ=/tmp/tachyon-wine/projects/$PN/work4k/02x_169
LOGS=/tmp/tachyon-wine/projects/$PN/logs
render_on() { echo "$(date +%H:%M:%S) start 02x 169 try 1 NF=1420" >> $LOGS/batch.log; touch $PROJ/r_0.log $PROJ/r_350.log; (bash -c "f1=744; f2=1080; : > $PROJ/r_733.log; : > $PROJ/r_1070.log; for i in \$(seq 1 400); do echo \"\$f1 \$i.0\" >> $PROJ/r_733.log; echo \"\$f2 \$i.0\" >> $PROJ/r_1070.log; f1=\$((f1+3)); f2=\$((f2+3)); touch $LOGS/batch.log; sleep 1; done" >/dev/null 2>&1 &) ; }
render_off() { pkill -f "r_733.log" || true; sleep 1; touch -d '2 hours ago' $PROJ/*.log $LOGS/*.log; }
cfg() { # compact extra-root extra-window
  printf "render_roots = ['Z:\\\\tmp\\\\tachyon-wine\\\\projects']\n%s\n[window]\ncompact = %s\nx = 560\ny = 40\nisland_cx = 800\nisland_y = 24\n%s\n" "$2" "$1" "$3" > $R/tachyon.toml; }
shot() { # name secs [env]
  (cd $R && env $3 TACHYON_DEBUG=1 TACHYON_DUMP_DIR='Z:\tmp\tachyon-work\run5' timeout $(( $2 + 4 )) wine tachyon.exe > $OUT/$1.log 2>&1 &)
  sleep $2; import -window root $OUT/$1_root.png; sleep 5; }
# companion: Hex at (1300,700) (1x atlas: 160 px window, hexagon centre at +80,+82); one shot
# without and one with the bubble (opened by a click on Hex)
hexshot() { # name gpu-extra
  cfg true "mode = 'companion'
[gpu]
$2" "hex_x = 1300
hex_y = 700"
  (cd $R && env TACHYON_DEBUG=1 TACHYON_DUMP_DIR='Z:\tmp\tachyon-work\run5' timeout 26 wine tachyon.exe > $OUT/$1.log 2>&1 &)
  sleep 12; import -window root $OUT/$1_root.png
  xdotool mousemove 1380 782 click 1; sleep 2; xdotool mousemove 1500 980; sleep 1
  import -window root $OUT/$1_bubble_root.png; sleep 14; }
case "$1" in
panel) render_off; cfg false "" ""; shot panel 16 ;;
panel_render) render_on; sleep 3; cfg false "" ""; shot panel_render 16; render_off ;;
island) render_off; cfg true "" ""; shot island_idle 12 ;;
island_render) render_on; sleep 3; cfg true "" ""; shot island_render 14; render_off ;;
showall) render_off; cfg false "" "show_all_processes = true"; shot showall 14 ;;
tint_opaque) render_off; cfg true "glass = 'tint'" ""; shot island_tint 12 ;;
hex_idle) render_off; hexshot hex_idle "temp_amber_c = 90
temp_red_c = 95" ;;
hex_render) render_on; sleep 3; hexshot hex_render "temp_amber_c = 90
temp_red_c = 95"; render_off ;;
hex_hot) render_off; hexshot hex_hot "temp_amber_c = 72
temp_red_c = 95" ;;
esac
