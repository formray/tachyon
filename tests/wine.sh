#!/bin/bash
# Wine smoke harness for Tachyon (build box only, never shipped).
# usage: wine.sh start | procs | run <dir> [env...] | stop
set -e
export WINEPREFIX=/tmp/tachyon-wine/wp WINEDEBUG=-all DISPLAY=:77
F=$WINEPREFIX/drive_c/rmfake
TT=/tmp/tachyon-work
case "$1" in
start)
  pgrep -f "Xvfb :77" >/dev/null || (Xvfb :77 -screen 0 1600x1000x24 +extension Composite >/dev/null 2>&1 &) ; sleep 1
  # busy wallpaper + a compositor, so layered per-pixel alpha is really blended (proves: no rectangle)
  hsetroot -fill ${WALL:-/tmp/tachyon-work/wall_demo.jpg} >/dev/null 2>&1 || true
  pgrep -x picom >/dev/null || (picom --backend xrender --no-fading-openclose --shadow-exclude "class_g ~= '.*'" >/dev/null 2>&1 &)
  cp $TT/nvml.dll $WINEPREFIX/drive_c/windows/system32/nvml.dll
  for p in Steam/steam.exe Game/Cyberpunk2077.exe ComfyUI/ComfyUI.exe ComfyUI/python_embeded/python.exe Tools/Helper.exe \
           py/python.exe ffmpeg/ffmpeg.exe OBS/obs64.exe Chrome/chrome.exe Win/dwm.exe; do cp $TT/sleeper.exe $F/$p; done
  rm -f $F/pids/*
  W='C:\rmfake'
  (wine "$W\\Steam\\steam.exe" "$W\\pids\\steam.pid" "$W\\Game\\Cyberpunk2077.exe" "$W\\pids\\game.pid" &)
  (wine "$W\\ComfyUI\\ComfyUI.exe" "$W\\pids\\comfy.pid" "$W\\ComfyUI\\python_embeded\\python.exe" "$W\\pids\\comfypy.pid" &)
  (wine "$W\\Tools\\Helper.exe" "$W\\pids\\helper.pid" "$W\\py\\python.exe" "$W\\pids\\py.pid" "$W\\ffmpeg\\ffmpeg.exe" "$W\\pids\\ffmpeg.pid" &)
  (wine "$W\\OBS\\obs64.exe" "$W\\pids\\obs.pid" &)
  (wine "$W\\Chrome\\chrome.exe" "$W\\pids\\chrome.pid" &)
  (wine "$W\\Win\\dwm.exe" "$W\\pids\\dwm.pid" &)
  for i in $(seq 1 40); do [ $(ls $F/pids | wc -l) -ge 10 ] && break; sleep 0.5; done
  ls $F/pids | wc -l
  $0 procs ;;
procs)
  pid() { tr -d '\r\n' < $F/pids/$1.pid; }
  printf "%s\n" "$(pid game) 6144 G 88 0 0" "$(pid comfypy) 7420 C 64 0 0" "$(pid py) 1210 C 21 0 0" \
     "$(pid obs) -1 G 3 41 18" "$(pid chrome) -1 G 6 0 4" "$(pid ffmpeg) 296 G 2 0 26" "$(pid dwm) -1 G 0 0 0" \
     "$(pid steam) -1 G 0 0 0" "$(pid helper) -1 G 1 0 0" > $F/procs.txt
  cat $F/procs.txt ;;
stop) wineserver -k || true; pkill -x picom || true; pkill -f "Xvfb :77" || true ;;
esac
