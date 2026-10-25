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
render_off
[ -n "$OLD" ] && python3 measure.py $OLD tachyon.toml true "old island (idle)"
python3 measure.py $NEW tachyon.toml true "island (idle)"
python3 measure.py $NEW tachyon.toml true "companion idle (breath + blinks)" "$COOL"
python3 measure.py $NEW tachyon.toml true "companion hot (3 fps loop)" "$HOT"
render_on
[ -n "$OLD" ] && python3 measure.py $OLD tachyon.toml true "old island (rendering)"
python3 measure.py $NEW tachyon.toml true "companion rendering (2 fps + edge)" "$COOL"
render_off
python3 measure.py $NEW tachyon.toml false "panel (idle)"
