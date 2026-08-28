import asyncio, sys, os, json, time
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

TESTS = [
    ("ping-pong",  "ping", {}, 10),
    ("network",    "http_request", {"url": "https://example.com", "method": "GET"}, 20),
    ("network",    "network_stats", {}, 10),
    ("database",   "db_set", {"key": "a", "value": "1"}, 10),
    ("database",   "db_get", {"key": "a"}, 10),
    ("database",   "db_incr", {"key": "cnt", "amount": 5}, 10),
    ("database",   "db_query", {"sql": "SELECT 40+2 AS answer"}, 10),
    ("notes",      "note_create", {"title": "t1", "body": "b1"}, 10),
    ("notes",      "note_list", {}, 10),
    ("calendar",   "event_create", {"title": "e1", "start_ms": int(time.time()*1000)+60000}, 10),
    ("calendar",   "event_list", {}, 10),
    ("secrets",    "secret_set", {"name": "x", "value": "v"}, 10),
    ("secrets",    "secret_get", {"name": "x"}, 10),
    ("filesystem", "fs_write", {"path": "/tmp/vyn_audit.txt", "text": "hi"}, 10),
    ("filesystem", "fs_read", {"path": "/tmp/vyn_audit.txt"}, 10),
    ("filesystem", "fs_read", {"path": "/etc/shadow"}, 10),
    ("gated-write","request_write", {"path": "/tmp/opencode/audit/data/gw/t.txt", "content": "g"}, 10),
    ("sync",       "sync_set", {"key": "k", "value": {"n": 1}}, 10),
    ("sync",       "sync_get_snapshot", {}, 10),
    ("sync-client","sync_client_get_state", {}, 10),
    ("notify",     "notify_send", {"title": "audit", "message": "test from live kernel"}, 15),
    ("clipboard",  "clipboard_providers", {}, 30),
    ("clipboard",  "clipboard_write", {"text": "vynkor-audit"}, 60),
    ("clipboard",  "clipboard_read", {}, 60),
    ("media",      "media_list_players", {}, 10),
    ("system",     "sys_info", {}, 10),
    ("system",     "sys_battery", {}, 10),
    ("system",     "sys_procs", {}, 10),
    ("ai",         "list_agents", {}, 10),
    ("search",     "web_search", {"query": "test", "provider": "brave", "api_key_env": "BRAVE_API_KEY"}, 20),
    ("tts",        "tts_voices", {"provider": "sherpa"}, 120),
    ("tts",        "tts_synthesize", {"provider": "sherpa", "text": "привет", "voice": "sid:0"}, 180),
    ("stt",        "stt_models", {"provider": "sherpa"}, 120),
    ("stt",        "stt_transcribe", {"provider": "sherpa",
        "audio_base64": __import__("base64").b64encode(
            open("/home/behzod/projects/vynkor-core/vynkor-plugins/models/stt/zipformer-ru-int8/test_wavs/0.wav","rb").read()).decode(),
        "format": "wav"}, 180),
]

async def main():
    pid = f"client-{os.getpid()}"
    c = VynkorWsClient("wss://127.0.0.1:8130/ws", mint(pid), SECRET, plugin_id=pid)
    await c.connect()
    print(f"connected as {pid}\n")
    ok = fail = 0
    for plugin, action, params, tmo in TESTS:
        try:
            r = await c.call(plugin, action, params, timeout=tmo)
            d = r.get("data") or {}
            has_payload = bool(d) or (r["status"] == "ACTION_OK" and not r.get("error"))
            good = has_payload and (r["status"] in ("ACTION_OK", "?"))
            ok += good; fail += (not good)
            mark = "OK " if good else "ERR"
            extra = ""
            d = r.get("data") or {}
            for k in ("reply", "text", "value", "content"):
                if isinstance(d.get(k), str):
                    extra = f' -> {d[k][:50]!r}'; break
            print(f"[{mark}] {plugin}.{action} {r['status']} {r.get('ms')}ms{extra}"
                  + (f" | {r['error'][:70]}" if r.get("error") else ""))
        except Exception as e:
            fail += 1
            print(f"[TMO] {plugin}.{action} {type(e).__name__}: {str(e)[:60]}")
    print(f"\n{ok} ok, {fail} err")
    await c.close()

asyncio.run(main())
