"""Minimal Vynkor WS client: register, call actions. Mirrors kernel integration-test protocol."""
import asyncio, json, struct, zlib, hmac, hashlib, time, uuid
import websockets
import sys
sys.path.insert(0, "/home/behzod/projects/vynkor-core/vynkor-sdk-python")
from vynkor import vynkor_protocol_pb2 as pb

MAGIC = 0x5652
FLAG_MAC_PRESENT = 0x0001
FLAG_COMPRESSED = 0x0002


def _derive_session_key(secret: bytes, nonce: bytes, plugin_id: str) -> bytes:
    """HKDF-SHA256(salt=nonce, ikm=secret, info=b'vynkor-frame-mac-v1|'+plugin_id) -> 32B"""
    from cryptography.hazmat.primitives.kdf.hkdf import HKDF
    from cryptography.hazmat.primitives import hashes
    hk = HKDF(algorithm=hashes.SHA256(), length=32, salt=nonce,
              info=b"vynkor-frame-mac-v1|" + plugin_id.encode())
    return hk.derive(secret)


def build_frame(target: str, payload: bytes, key: bytes | None = None) -> bytes:
    flags = FLAG_MAC_PRESENT if key else 0
    tgt = target.encode()[:32].ljust(32, b"\0")
    crc = zlib.crc32(payload) & 0xFFFFFFFF
    head = struct.pack(">HHI32sI", MAGIC, flags, len(payload), tgt, crc)
    out = head + payload
    if key:
        tag = hmac.new(key, head + payload, hashlib.sha256).digest()
        out += tag
    return out


def parse_frame(data: bytes):
    magic, flags, length, tgt, crc = struct.unpack(">HHI32sI", data[:44])
    assert magic == MAGIC, f"bad magic {magic:#x}"
    payload = data[44:44 + length]
    mac = data[44 + length:44 + length + 32] if flags & FLAG_MAC_PRESENT else None
    return tgt.rstrip(b"\0").decode(), flags, payload, crc, mac


def _maybe_decompress(payload: bytes, flags: int) -> bytes:
    if flags & FLAG_COMPRESSED:
        import zstandard
        return zstandard.ZstdDecompressor().decompress(payload)
    return payload


class VynkorWsClient:
    def __init__(self, url: str, jwt: str, secret: str, plugin_id="audit-runner"):
        self.url, self.jwt, self.secret, self.plugin_id = url, jwt, secret, plugin_id
        self.key = None
        self.ws = None

    async def connect(self):
        import ssl
        localhost_dev_tls = ssl.create_default_context()
        localhost_dev_tls.check_hostname = False
        localhost_dev_tls.verify_mode = ssl.CERT_NONE
        self.ws = await websockets.connect(
            self.url.replace("ws://", "wss://"), subprotocols=["vynkor", self.jwt],
            max_size=512 * 1024 * 1024, ssl=localhost_dev_tls)
        reg = pb.Envelope(plugin_register=pb.PluginRegister(
            plugin_id=self.plugin_id,
            version="0.1.0",
            description="audit runner",
            manifest=pb.PluginManifest(
                permissions=[name for name in dir(pb.PermissionType)
                             if name.startswith("PERMISSION_")
                             and isinstance(getattr(pb.PermissionType, name), int)],
                ipc_targets=["*"],
            ),
            jwt_token=self.jwt,
        ))
        await self.ws.send(build_frame("kernel", reg.SerializeToString()))
        raw = await asyncio.wait_for(self.ws.recv(), timeout=10)
        _, _, payload, _, _ = parse_frame(raw)
        env = pb.Envelope(); env.ParseFromString(payload)
        ack = env.plugin_register_ack
        if not ack.accepted:
            raise RuntimeError(f"registration rejected: {ack.reject_reason}")
        if ack.session_nonce:
            self.key = _derive_session_key(
                self.secret.encode(), bytes(ack.session_nonce), self.plugin_id)

    async def call(self, plugin: str, action: str, params: dict | None = None,
                   timeout: float = 60.0) -> dict:
        req = pb.Envelope(action_request=pb.ActionRequest(
            action_id=str(uuid.uuid4()),
            action=action,
            params_json=json.dumps(params or {}).encode(),
            timeout_ms=int(timeout * 1000),
        ))
        t0 = time.perf_counter()
        await self.ws.send(build_frame("kernel", req.SerializeToString(), self.key))
        while True:
            raw = await asyncio.wait_for(self.ws.recv(), timeout=timeout + 15)
            if not isinstance(raw, (bytes, bytearray, memoryview)):
                continue
            _, flags, payload, _, _ = parse_frame(raw)
            env = pb.Envelope()
            try:
                env.ParseFromString(_maybe_decompress(payload, flags))
            except Exception:
                continue
            if env.WhichOneof("payload") == "action_response" \
                    and env.action_response.action_id == req.action_request.action_id:
                r = env.action_response
                dt = (time.perf_counter() - t0) * 1000
                status = pb.ActionStatus.Name(r.status) if r.status else "?"
                raw_json = r.data_json.decode("utf-8", "replace") if r.data_json else ""
                try:
                    data = json.loads(raw_json) if raw_json else {}
                except json.JSONDecodeError:
                    data = {"_raw": raw_json[:2000]}
                return {"status": status, "data": data,
                        "error": r.error or None, "ms": round(dt, 1)}
            if env.WhichOneof("payload") == "error":
                e = env.error
                dt = (time.perf_counter() - t0) * 1000
                try:
                    code = pb.ErrorCode.Name(e.code)
                except ValueError:
                    code = str(e.code)
                return {"status": f"KERNEL_{code}", "data": {},
                        "error": e.message, "ms": round(dt, 1)}

    async def close(self):
        if self.ws:
            await self.ws.close()
