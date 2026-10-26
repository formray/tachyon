#!/bin/bash
# CPU/RSS of each mode (wine + Xvfb + picom, fake NVML; build box only). Assumes wine.sh start has run.
# Set OLD=<path to an older tachyon.exe> to measure it alongside.
cd "$(dirname "$0")"
NEW=${NEW:-../target/x86_64-pc-windows-gnu/release/tachyon.exe}; OLD=${OLD:-}
PROJ=/tmp/tachyon-wine/projects/demo4k/work4k/02x_169; LOGS=/tmp/tachyon-wine/projects/demo4k/logs
render_on() { echo "$(date +%H:%M:%S) start 02x 169 try 1 NF=1420" >> $LOGS/batch.log; touch $PROJ/r_0.log $PROJ/r_350.log; (bash -c "f1=744; f2=1080; : > $PROJ/r_733.log; : > $PROJ/r_1070.log; for i in \$(seq 1 400); do echo \"\$f1 \$i.0\" >> $PROJ/r_733.log; echo \"\$f2 \$i.0\" >> $PROJ/r_1070.log; f1=\$((f1+3)); f2=\$((f2+3)); touch $LOGS/batch.log; sleep 1; done" >/dev/null 2>&1 &) ; sleep 3; }
render_off() { pkill -f "r_733.log" || true; sleep 1; touch -d '2 hours ago' $PROJ/*.log $LOGS/*.log; }
COOL="mode = 'companion'\n[gpu]\ntemp_amber_c = 90\ntemp_red_c = 95"
HOT="mode = 'companion'\n[gpu]\ntemp_amber_c = 72\ntemp_red_c = 95"
# m <compact> <label> [extra toml]: the new exe, and the old one alongside when OLD is set
m() { [ -n "$OLD" ] && python3 measure.py $OLD tachyon.toml "$1" "old $2" "$3"; python3 measure.py $NEW tachyon.toml "$1" "$2" "$3"; }
ISL="mode = 'island'"; PAN="mode = 'panel'"
rm -f /tmp/tachyon-wine/wp/drive_c/rmfake/scenario.txt
render_off
m true "island (idle)" "$ISL"
m true "companion idle (breath + blinks)" "$COOL"
[ -z "$QUICK" ] && python3 measure.py $NEW tachyon.toml true "companion hot (3 fps loop)" "$HOT"
m false "panel (idle)" "$PAN"
render_on
m true "island (rendering)" "$ISL"
m true "companion rendering (2 fps + edge)" "$COOL"
m false "panel (rendering)" "$PAN"
render_off
