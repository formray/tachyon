#!/usr/bin/env python3
"""CPU/RSS under wine (build box only): measure.py <exe> <cfgname> <compact true|false> <label>.
22 s warm-up, then a 30 s window. App = the wine process whose /proc comm is the exe name."""
import os, sys, time, shutil, subprocess, glob
exe, cfg, compact, label = sys.argv[1:5]
extra = sys.argv[5].replace("\\n", "\n") + "\n" if len(sys.argv) > 5 else ""  # e.g. "mode = 'companion'"
env = dict(os.environ, WINEPREFIX=os.environ.get("WINEPREFIX", "/tmp/tachyon-wine/wp"), WINEDEBUG="-all", DISPLAY=os.environ.get("TDISPLAY", ":77"))
d = os.environ.get("TT", "/tmp/tachyon-work") + "/meas5"; shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
name = os.path.basename(exe); shutil.copy(exe, d)
open(f"{d}/{cfg}", "w").write("render_roots = ['Z:\\\\tmp\\\\tachyon-wine\\\\projects']\n" + extra + "[window]\n"
    f"compact = {compact}\nx = 560\ny = 40\nisland_cx = 800\nisland_y = 24\nalways_on_top = false\n")
p = subprocess.Popen(["wine", name], cwd=d, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(22)
def find(word):
    for s in glob.glob("/proc/[0-9]*/cmdline"):
        try:
            argv0 = open(s, "rb").read().split(b"\0")[0].decode(errors="ignore")
            if argv0.replace("\\", "/").split("/")[-1] == word: return int(s.split("/")[2])
        except OSError: pass
app, ws = find(name), find("wineserver64") or find("wineserver")
hz = os.sysconf("SC_CLK_TCK")
def cpu(pid): f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split(); return int(f[11]) + int(f[12])
a0, w0 = cpu(app), cpu(ws); time.sleep(30); a1, w1 = cpu(app), cpu(ws)
rss = [int(l.split()[1]) for l in open(f"/proc/{app}/status") if l.startswith("VmRSS")][0]
print(f"{label}: app CPU {(a1-a0)*100/hz/30:.2f} %  wineserver CPU {(w1-w0)*100/hz/30:.2f} %  RSS {rss/1024:.0f} MB  exe {os.path.getsize(exe):,} B", flush=True)
os.kill(app, 9)
try: p.wait(timeout=20)
except subprocess.TimeoutExpired: p.kill()
time.sleep(3)
