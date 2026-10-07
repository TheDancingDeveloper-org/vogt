"""Measure a live call's latency against a deployment, with no microphone.

Places a call on ``/api/assistant/call`` (WI-960), speaks a turn into it from
audio — a 16 kHz mono PCM16 WAV file, or a sentence synthesized by the
deployment's own ``/api/assistant/tts`` — paced in real time, and reports how
long it took from the end of that speech to the first reply audio arriving,
next to the server's own per-stage ``metrics``::

    VOGT_URL=https://vogt.example VOGT_TOKEN=... \\
        uv run python scripts/call_latency.py --say "what is running" --turns 3

The turns are real assistant turns: they land in the assistant's conversation
like any other. ``--reset`` clears the conversation afterwards.

Standard library only (a minimal RFC 6455 client below), so it runs anywhere
the repository's Python does.
"""

from __future__ import annotations

import argparse
import array
import base64
import json
import os
import socket
import ssl
import struct
import sys
import time
import urllib.request
import wave
from dataclasses import dataclass, field
from io import BytesIO
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

RATE = 16_000
FRAME_SAMPLES = 320  # 20 ms


class WebSocket:
    """Just enough of a WebSocket client: text and binary frames, masked."""

    def __init__(self, url: str) -> None:
        parsed = urlparse(url)
        secure = parsed.scheme in ("https", "wss")
        port = parsed.port or (443 if secure else 80)
        host = parsed.hostname or "localhost"
        raw = socket.create_connection((host, port), timeout=30)
        if secure:
            context = ssl.create_default_context()
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            self.sock: socket.socket = context.wrap_socket(raw, server_hostname=host)
        else:
            self.sock = raw
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET {parsed.path or '/'} HTTP/1.1\r\nHost: {host}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(request.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(1)
            if not chunk:
                raise ConnectionError("closed during handshake")
            head += chunk
        status = head.split(b"\r\n", 1)[0].decode()
        if " 101 " not in status:
            raise ConnectionError(f"upgrade refused: {status}")

    def send(self, payload: bytes, opcode: int) -> None:
        header = bytearray([0x80 | opcode])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < 65_536:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(bytes(header) + mask + masked)

    def send_json(self, event: dict[str, Any]) -> None:
        self.send(json.dumps(event).encode(), 0x1)

    def _exact(self, n: int) -> bytes:
        out = b""
        while len(out) < n:
            chunk = self.sock.recv(n - len(out))
            if not chunk:
                raise ConnectionError("socket closed")
            out += chunk
        return out

    def recv(self, timeout: float) -> tuple[int, bytes] | None:
        """One whole message, or ``None`` if nothing arrived in time."""
        self.sock.settimeout(timeout)
        try:
            first = self._exact(2)
        except TimeoutError:
            return None
        self.sock.settimeout(30)
        opcode = first[0] & 0x0F
        length = first[1] & 0x7F
        if length == 126:
            length = struct.unpack(">H", self._exact(2))[0]
        elif length == 127:
            length = struct.unpack(">Q", self._exact(8))[0]
        payload = self._exact(length)
        if opcode == 0x9:  # ping
            self.send(payload, 0xA)
            return self.recv(timeout)
        return opcode, payload


def resample(samples: array.array[int], rate: int) -> array.array[int]:
    """Linear resampling to 16 kHz — plenty for a VAD and a transcriber."""
    if rate == RATE:
        return samples
    ratio = rate / RATE
    out = array.array("h")
    n = int(len(samples) / ratio)
    for i in range(n):
        pos = i * ratio
        j = int(pos)
        frac = pos - j
        a = samples[j]
        b = samples[min(j + 1, len(samples) - 1)]
        out.append(int(a + (b - a) * frac))
    return out


def read_wav(data: bytes) -> array.array[int]:
    with wave.open(BytesIO(data)) as wav:
        if wav.getsampwidth() != 2:
            raise SystemExit("audio must be 16-bit PCM WAV")
        frames = array.array("h", wav.readframes(wav.getnframes()))
        channels = wav.getnchannels()
        if channels > 1:
            frames = array.array("h", frames[::channels])
        return resample(frames, wav.getframerate())


def synthesize(base: str, token: str, text: str) -> array.array[int]:
    request = urllib.request.Request(
        f"{base}/api/assistant/tts",
        data=json.dumps({"text": text}).encode(),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        body: bytes = response.read()
    if not body.startswith(b"RIFF"):
        raise SystemExit("the deployment's TTS is not WAV; pass --wav instead")
    return read_wav(body)


@dataclass
class Turn:
    client_ms: float | None = None
    status: str = ""
    metrics: dict[str, Any] = field(default_factory=dict)
    transcript: str = ""
    reply: str = ""


def run_turn(ws: WebSocket, speech: array.array[int], realtime: bool) -> Turn:
    """Speak one turn and wait for its response to finish."""
    lead = array.array("h", [0] * (RATE * 3 // 10))
    tail = array.array("h", [0] * (RATE * 3 // 2))
    turn = Turn()
    speech_end = 0.0
    first_audio: float | None = None
    next_frame = time.monotonic()
    for part, voiced in ((lead, False), (speech, True), (tail, False)):
        for at in range(0, len(part), FRAME_SAMPLES):
            ws.send(part[at : at + FRAME_SAMPLES].tobytes(), 0x2)
            if realtime:
                next_frame += FRAME_SAMPLES / RATE
                time.sleep(max(0.0, next_frame - time.monotonic()))
            # Anything that has arrived meanwhile — the reply may start
            # while the trailing silence is still being sent.
            while (message := ws.recv(0.001)) is not None:
                first_audio = _note(ws, message, turn, first_audio)
        if voiced:
            speech_end = time.monotonic()
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline and not turn.status:
        message = ws.recv(1.0)
        if message is not None:
            first_audio = _note(ws, message, turn, first_audio)
    if first_audio is not None:
        turn.client_ms = (first_audio - speech_end) * 1000
    return turn


def _note(
    ws: WebSocket, message: tuple[int, bytes], turn: Turn, first_audio: float | None
) -> float | None:
    opcode, payload = message
    if opcode == 0x2:
        return first_audio if first_audio is not None else time.monotonic()
    if opcode != 0x1:
        return first_audio
    event = json.loads(payload)
    kind = event.get("type")
    if kind == "response.audio.start":
        ws.send_json(
            {
                "type": "output_audio.started",
                "response_id": event["response_id"],
                "index": event["index"],
            }
        )
    elif kind == "conversation.item.input_audio_transcription.completed":
        turn.transcript = event["text"]
    elif kind == "response.done":
        turn.status = event["status"]
        turn.metrics = event.get("metrics", {})
        turn.reply = event.get("text") or ""
        ws.send_json({"type": "output_audio.idle", "response_id": event["response_id"]})
    elif kind == "error":
        print(f"  server error: {event.get('message')}", file=sys.stderr)
    return first_audio


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--wav", help="16-bit PCM WAV of the turn to speak")
    source.add_argument("--say", help="text the deployment's TTS speaks as the turn")
    parser.add_argument("--turns", type=int, default=3)
    parser.add_argument(
        "--fast", action="store_true", help="send audio faster than real time"
    )
    parser.add_argument(
        "--reset", action="store_true", help="reset the conversation after"
    )
    args = parser.parse_args()
    base = os.environ.get("VOGT_URL", "").rstrip("/")
    token = os.environ.get("VOGT_TOKEN", "")
    if not base or not token:
        raise SystemExit("set VOGT_URL and VOGT_TOKEN")

    if args.wav:
        speech = read_wav(Path(args.wav).read_bytes())
    else:
        speech = synthesize(base, token, args.say)

    ws = WebSocket(base.replace("http", "ws", 1) + "/api/assistant/call")
    ws.send_json({"type": "auth", "token": token})
    opened = ws.recv(10.0)
    if opened is None or b"session.created" not in opened[1]:
        raise SystemExit(f"call refused: {opened!r}")

    results: list[Turn] = []
    for n in range(args.turns):
        turn = run_turn(ws, speech, realtime=not args.fast)
        results.append(turn)
        m = turn.metrics
        client = f"{turn.client_ms:.0f}" if turn.client_ms is not None else "-"
        print(
            f"turn {n + 1}: {turn.status or 'no response'}  client {client} ms  "
            f"server {m.get('speech_end_to_first_audio_ms', '-')} ms  "
            f"[endpoint {m.get('endpoint_ms', '-')}  stt {m.get('stt_ms', '-')}  "
            f"llm {m.get('llm_first_text_ms', '-')}  tts {m.get('tts_first_ms', '-')}  "
            f"tools {m.get('tool_rounds', 0)}{' filler' if m.get('filler') else ''}]"
        )
        print(f"  heard: {turn.transcript!r}  replied: {turn.reply[:80]!r}")
        time.sleep(1.0)
    measured = sorted(t.client_ms for t in results if t.client_ms is not None)
    if measured:
        print(
            "end of speech -> first audio (client): "
            f"median {measured[len(measured) // 2]:.0f} ms, "
            f"min {measured[0]:.0f}, max {measured[-1]:.0f}, n={len(measured)}"
        )
    if args.reset:
        request = urllib.request.Request(
            f"{base}/api/assistant/reset",
            headers={"Authorization": f"Bearer {token}"},
            method="POST",
        )
        urllib.request.urlopen(request, timeout=30).close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
