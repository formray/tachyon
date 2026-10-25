#!/bin/bash
# Interaction smoke test (wine + xdotool, build box only): click on the shadow padding of the
# island does nothing (click-through), island click -> panel, top-band double-click -> island,
# island drag -> position saved, panel close button.
export WINEPREFIX=/tmp/tachyon-wine/wp WINEDEBUG=-all DISPLAY=:77
R=/tmp/tachyon-work/run5; O=/tmp/tachyon-work/shots5; mkdir -p $R $O
pkill -f tachyon.exe; sleep 1
cp $(dirname "$0")/../target/x86_64-pc-windows-gnu/release/tachyon.exe $R/
printf "[window]\ncompact = true\nx = 560\ny = 40\nisland_cx = 800\nisland_y = 24\n" > $R/tachyon.toml
(cd $R && TACHYON_DEBUG=1 timeout 70 wine tachyon.exe > $O/interact.log 2>&1 &)
sleep 8
geo() { xdotool search --name '^Tachyon$' getwindowgeometry 2>/dev/null | tr '\n' ' '; echo; }
echo "start (island):        $(geo)"
# island capsule spans y 24..70; the window adds an 18 px shadow pad. y=12 is padding above the capsule.
xdotool mousemove 800 12 click 1; sleep 1.5
echo "after padding click:   $(geo)"
xdotool mousemove 800 46 click 1; sleep 1.5
echo "after island click:    $(geo)"; import -window root $O/i_after_click.png
xdotool mousemove 820 60 click --repeat 2 --delay 80 1; sleep 1.5
echo "after top dbl-click:   $(geo)"; import -window root $O/i_after_dblclick.png
xdotool mousemove 800 46 mousedown 1; sleep 0.2; xdotool mousemove 820 50; sleep 0.2; xdotool mousemove 900 300; sleep 0.3; xdotool mouseup 1; sleep 1.5
echo "after drag:            $(geo)"; grep -E "island_(cx|y)|compact" $R/tachyon.toml | tr '\n' ' '; echo
xdotool mousemove 900 300 click 1; sleep 1.5; PX=$(xdotool search --name "^Tachyon$" getwindowgeometry | sed -n "s/.*Position: \([0-9]*\),\([0-9]*\).*/\1 \2/p")
echo "panel again:           $(geo)"
# close button: 36 px from the right edge of the 480 px panel, 30 px below its top (30 px shadow pad)
set -- $PX; xdotool mousemove $(($1+30+480-24-12)) $(($2+30+30)) click 1; sleep 3
echo "after close button:    $(geo)  (empty = closed)"
