#!/usr/bin/env python3
"""A tiny RTSP server that loops a video as a live camera, using only ffmpeg and the standard library.

    python3 tools/rtsp_demo.py videos/demo.mp4                # -> rtsp://127.0.0.1:8554/highway
    python3 tools/rtsp_demo.py videos/demo.mp4 --start 95 --path south --port 8555

ffmpeg paces the clip in real time (-re) and emits RTP; this script answers the RTSP handshake
(OPTIONS / DESCRIBE / SETUP / PLAY) and relays the RTP packets to every connected client over TCP
(interleaved). Good enough to test Chira against a real RTSP source without a camera or an RTSP server install.
"""
import argparse, os, re, socket, subprocess, sys, tempfile, threading, time

ap = argparse.ArgumentParser()
ap.add_argument("video")
ap.add_argument("--port", type=int, default=8554)
ap.add_argument("--path", default="highway")
ap.add_argument("--start", type=int, default=0, help="start offset in seconds")
ap.add_argument("--rtp-port", type=int, default=0, help="local UDP port ffmpeg sends RTP to (default: pick a free one)")
args = ap.parse_args()

# 1. ffmpeg -> RTP on a local UDP port (packets are relayed to RTSP clients below)
rtp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
rtp.bind(("127.0.0.1", args.rtp_port))
rtp_port = rtp.getsockname()[1]
sdp_file = os.path.join(tempfile.mkdtemp(prefix="chira-rtsp-"), "stream.sdp")
ff = subprocess.Popen(["ffmpeg", "-loglevel", "error", "-re", "-stream_loop", "-1", "-ss", str(args.start), "-i", args.video, "-an",
                       "-c:v", "copy", "-f", "rtp", "-sdp_file", sdp_file, f"rtp://127.0.0.1:{rtp_port}"], stdin=subprocess.DEVNULL)
for _ in range(100):
    if os.path.exists(sdp_file) and os.path.getsize(sdp_file) > 20:
        break
    time.sleep(0.1)
else:
    sys.exit("ffmpeg did not produce an SDP")
sdp = open(sdp_file).read()
sdp = re.sub(r"m=video \d+", "m=video 0", sdp)
sdp = sdp.replace("a=rtpmap", "a=control:streamid=0\r\na=rtpmap", 1).replace("\r\n", "\n").replace("\n", "\r\n")
# a 'c=' / 'o=' line with the loopback address is fine for TCP-interleaved clients

clients, lock = [], threading.Lock()          # sockets that sent PLAY


def relay():
    while True:
        pkt, _ = rtp.recvfrom(65536)
        frame = b"$\x00" + len(pkt).to_bytes(2, "big") + pkt
        with lock:
            for c in list(clients):
                try:
                    c.sendall(frame)
                except OSError:
                    clients.remove(c)


def serve(conn):
    buf = b""
    session = f"{id(conn) & 0xffffff:06x}"
    try:
        while True:
            while b"\r\n\r\n" not in buf:
                d = conn.recv(4096)
                if not d:
                    return
                buf += d
            head, buf = buf.split(b"\r\n\r\n", 1)
            lines = head.decode(errors="replace").split("\r\n")
            method = lines[0].split()[0]
            h = {k.lower(): v.strip() for k, v in (l.split(":", 1) for l in lines[1:] if ":" in l)}
            cseq = h.get("cseq", "0")
            out = ["RTSP/1.0 200 OK", f"CSeq: {cseq}"]
            body = ""
            if method == "OPTIONS":
                out.append("Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN")
            elif method == "DESCRIBE":
                body = sdp
                out += ["Content-Type: application/sdp", f"Content-Length: {len(body)}"]
            elif method == "SETUP":
                out += [f"Session: {session}", "Transport: RTP/AVP/TCP;unicast;interleaved=0-1"]
            elif method == "PLAY":
                out += [f"Session: {session}", "Range: npt=0.000-"]
                with lock:
                    if conn not in clients:
                        clients.append(conn)
            elif method == "TEARDOWN":
                out.append(f"Session: {session}")
            conn.sendall(("\r\n".join(out) + "\r\n\r\n" + body).encode())
            if method == "TEARDOWN":
                return
    except OSError:
        pass
    finally:
        with lock:
            if conn in clients:
                clients.remove(conn)
        conn.close()


threading.Thread(target=relay, daemon=True).start()
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("0.0.0.0", args.port))
srv.listen(8)
print(f"rtsp://127.0.0.1:{args.port}/{args.path}   (ctrl-c to stop)", flush=True)
try:
    while True:
        c, _ = srv.accept()
        threading.Thread(target=serve, args=(c,), daemon=True).start()
finally:
    ff.terminate()
