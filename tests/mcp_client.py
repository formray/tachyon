#!/usr/bin/env python3
"""Scripted MCP client for `tachyon.exe mcp` under wine (build box only).
usage: mcp_client.py <dir with tachyon.exe and tachyon.toml> [expect_source]
Runs the handshake, lists the tools, calls each one and checks the answers. Exit code 0 = pass."""
import json, os, subprocess, sys, time
d = sys.argv[1]; expect = sys.argv[2] if len(sys.argv) > 2 else None
env = dict(os.environ, WINEPREFIX="/tmp/tachyon-wine/wp", WINEDEBUG="-all", DISPLAY=":77")
p = subprocess.Popen(["wine", "tachyon.exe", "mcp"], cwd=d, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
n = 0
def rpc(method, params=None, notify=False):
    global n
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None: msg["params"] = params
    if not notify:
        n += 1; msg["id"] = n
    p.stdin.write(json.dumps(msg) + "\n"); p.stdin.flush()
    if notify: return None
    t0 = time.time(); line = p.stdout.readline()
    r = json.loads(line); assert r.get("id") == n, r
    return r, time.time() - t0
fails = []
def check(cond, what):
    print(("  ok   " if cond else "  FAIL ") + what); cond or fails.append(what)
r, dt = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "rig", "version": "1"}})
check(r["result"]["protocolVersion"] == "2025-06-18" and r["result"]["serverInfo"]["name"] == "tachyon", f"initialize ({dt*1000:.0f} ms, server {r['result']['serverInfo']['version']})")
rpc("notifications/initialized", notify=True)
r, _ = rpc("tools/list", {})
names = [t["name"] for t in r["result"]["tools"]]
check(names == ["get_status", "get_bottleneck", "get_vram_forecast", "get_processes", "get_blackbox", "get_history", "compare_last_run"], f"tools/list: {', '.join(names)}")
check(all(t["annotations"]["readOnlyHint"] for t in r["result"]["tools"]), "every tool is marked read-only")
out = {}
for name, args in [("get_status", {}), ("get_bottleneck", {}), ("get_vram_forecast", {}), ("get_processes", {}),
                   ("get_blackbox", {"minutes": 2}), ("get_history", {"n": 5}), ("compare_last_run", {})]:
    r, dt = rpc("tools/call", {"name": name, "arguments": args})
    res = r["result"]; out[name] = res
    sc = res.get("structuredContent", {})
    summary = res["content"][0]["text"].replace("\n", " ")[:110]
    check("content" in res, f"{name} ({dt*1000:.0f} ms, isError={res['isError']}): {summary}")
if expect == "off":
    check(all(out[k]["isError"] for k in out), "agent_api = false: every call explains how to turn it on")
else:
    s = out["get_status"]["structuredContent"]
    check(not out["get_status"]["isError"], "get_status answers")
    check(s["source"] == expect, f"source is {s['source']} (expected {expect})")
    check(s["gpu"]["name"] == "FAKE GeForce RTX 5080", f"gpu {s['gpu']['name']} driver {s['gpu']['driver']}")
    check(s["reading"]["util"] is not None and s["reading"]["energy_mj"] is not None and s["reading"]["pcie_max"] is not None,
          f"reading: util {s['reading']['util']}% power {s['reading']['power_w']} W energy {s['reading']['energy_mj']} mJ pcie max {s['reading']['pcie_max']} MB/s cpu {s['reading']['cpu']}")
    b = out["get_bottleneck"]["structuredContent"]["bottleneck"]
    check(b is not None and b.get("sentence"), f"bottleneck: {b and b['sentence']}")
    procs = out["get_processes"]["structuredContent"]["processes"]
    check(len(procs) > 0, f"processes: {', '.join(x['name'] for x in procs)}")
    h = out["get_history"]["structuredContent"]
    check("jobs" in h, f"history: {h['count']} jobs")
    bb = out["get_blackbox"]["structuredContent"]
    check("peaks" in bb, f"blackbox: {bb['samples_in_window']} samples in 2 min")
r, _ = rpc("ping", {}); check(r["result"] == {}, "ping")
r, _ = rpc("resources/list", {}); check(r["error"]["code"] == -32601, "unknown method -> -32601")
p.stdin.write("not json\n"); p.stdin.flush(); r = json.loads(p.stdout.readline()); check(r["error"]["code"] == -32700, "bad JSON -> -32700")
p.stdin.close(); p.wait(timeout=20); check(p.returncode == 0, f"server exits when stdin closes (code {p.returncode})")
print("PASS" if not fails else f"FAILED: {fails}"); sys.exit(1 if fails else 0)
