import asyncio, os, sys, time, json
sys.path.insert(0, "/tmp/opencode/audit")
from vynkor_ws import VynkorWsClient
import hmac, hashlib, base64 as b

SECRET = "audit-test-secret-0123456789abcdef0123456789abcdef"
TARGETS = ["ping-pong", "network", "ai", "database", "tts", "stt", "secrets",
           "gated-write", "notify", "sync", "sync-client", "notes", "calendar",
           "media", "clipboard", "filesystem", "search", "system"]


def mint(sub):
    def b64u(x): return b.urlsafe_b64encode(x).rstrip(b"=").decode()
    h = b64u(json.dumps({"alg": "HS256", "typ": "JWT"}).encode()); now = int(time.time())
    claims = {"sub": sub, "permissions": ["ipc_send"] + TARGETS, "ipc_targets": TARGETS,
              "exp": now + 3600, "iat": now}
    c = b64u(json.dumps(claims).encode())
    sig = hmac.new(SECRET.encode(), f"{h}.{c}".encode(), hashlib.sha256).digest()
    return f"{h}.{c}.{b64u(sig)}"

PAGE = os.sysconf("SC_PAGE_SIZE") // 1024


def snap(names):
    out = {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            comm = open(f"/proc/{pid}/comm").read().strip()
        except OSError:
            continue
        if comm not in names:
            continue
        try:
            parts = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
            rss = int(parts[21]) * PAGE
            cpu = int(parts[11]) + int(parts[12])
            out[comm] = (rss, cpu)
        except (OSError, IndexError, ValueError):
            continue
    return out


WATCH = {"vyn", "database", "network", "sync", "notes"}


async def main():
    hz = os.sysconf("SC_CLK_TCK")
    pid = f"load-{os.getpid()}"
    c = VynkorWsClient("wss://127.0.0.1:8130/ws", mint(pid), SECRET, plugin_id=pid)
    await c.connect()

    t0 = time.perf_counter()
    before = snap(WATCH)
    peak = {k: v[0] for k, v in before.items()}

    async def sampler():
        nonlocal peak
        while not done:
            for k, (rss, _) in snap(WATCH).items():
                peak[k] = max(peak.get(k, 0), rss)
            await asyncio.sleep(0.2)

    done = False
    st = asyncio.create_task(sampler())

    ok = err = 0
    lat = []
    for i in range(60):
        r = await c.call("database", "db_set", {"key": f"load:{i}", "value": "x" * 512}, timeout=15)
        ok += r["status"] == "ACTION_OK"; err += r["status"] != "ACTION_OK"
        lat.append(r["ms"])
        r = await c.call("database", "db_get", {"key": f"load:{i}"}, timeout=15)
        ok += r["status"] == "ACTION_OK"; err += r["status"] != "ACTION_OK"
        lat.append(r["ms"])
    for i in range(5):
        r = await c.call("network", "http_request",
                         {"url": "https://example.com", "method": "GET"}, timeout=20)
        ok += r["status"] == "ACTION_OK"; err += r["status"] != "ACTION_OK"
    wall = time.perf_counter() - t0
    await asyncio.sleep(0.5)
    done = True
    await st

    after = snap(WATCH)
    print(f"load phase: 125 db ops + 5 https GET in {wall:.1f}s "
          f"({ok} ok / {err} err), db op avg {sum(lat)/len(lat):.2f} ms\n")
    print(f"{'proc':<10} {'rss_before':>10} {'rss_after':>10} {'rss_peak':>10} {'cpu_delta':>9}")
    for k in sorted(after):
        b = before.get(k, (0, 0)); a = after[k]
        cpu = (a[1] - b[1]) / hz
        print(f"{k:<10} {b[0]/1024:>8.1f}M {a[0]/1024:>8.1f}M {peak[k]/1024:>8.1f}M {cpu:>7.2f}s")
    await c.close()


asyncio.run(main())
