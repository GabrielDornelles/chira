#!/usr/bin/env python3
"""Regenerate the README screenshots + the hero GIF from a running Chira server.

    DATA_DIR=/tmp/chira-docs ./run.sh &         # a clean data dir, normal (non-developer) mode
    python3 tools/capture.py --warm 90          # adds the two demo cameras, waits, captures into docs/img

Needs: google-chrome, `pip install websockets pillow`. Drives headless Chrome through the DevTools protocol.
"""
import argparse, asyncio, base64, io, json, os, subprocess, sys, time, urllib.request

import websockets
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("--url", default="http://localhost:8787")
ap.add_argument("--out", default="docs/img")
ap.add_argument("--warm", type=int, default=75, help="seconds to let the cameras run before capturing (fills analytics)")
ap.add_argument("--gif-seconds", type=float, default=5.0)
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
PORT = 9333
PAT = "remote-debugging-" + f"port={PORT}"        # (kept split so a shell one-liner containing it never self-matches)


def api(method, path, body=None):
    req = urllib.request.Request(args.url + path, data=json.dumps(body).encode() if body is not None else None, method=method, headers={"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req))


class Browser:
    async def start(self, w, h):
        subprocess.run(["pkill", "-f", PAT])
        time.sleep(0.5)
        self.prof = f"/tmp/chira-capture-{int(time.time())}"
        # no DISPLAY / D-Bus: on a desktop session Chrome 136 can hang waiting for them in headless mode
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "DBUS_SESSION_BUS_ADDRESS")}
        self.proc = subprocess.Popen(["google-chrome", "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars", f"--window-size={w},{h}", f"--{PAT}", f"--user-data-dir={self.prof}", "about:blank"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
        for _ in range(60):
            try:
                tabs = json.load(urllib.request.urlopen(f"http://localhost:{PORT}/json"))
                break
            except Exception:
                time.sleep(0.2)
        self.ws = await websockets.connect([t for t in tabs if t["type"] == "page"][0]["webSocketDebuggerUrl"], max_size=2**27)
        self.n = 0
        await self.cmd("Runtime.enable")
        await self.size(w, h)

    async def cmd(self, method, **params):
        self.n += 1
        await self.ws.send(json.dumps({"id": self.n, "method": method, "params": params}))
        while True:
            r = json.loads(await self.ws.recv())
            if r.get("id") == self.n:
                return r.get("result", {})

    async def size(self, w, h):
        await self.cmd("Emulation.setDeviceMetricsOverride", width=w, height=h, deviceScaleFactor=1, mobile=False)

    async def js(self, expr):
        r = await self.cmd("Runtime.evaluate", expression=expr, returnByValue=True, awaitPromise=True)
        return r.get("result", {}).get("value")

    async def move(self, x, y):
        await self.cmd("Input.dispatchMouseEvent", type="mouseMoved", x=x, y=y)

    async def click(self, x, y):
        await self.move(x, y)
        for t in ("mousePressed", "mouseReleased"):
            await self.cmd("Input.dispatchMouseEvent", type=t, x=x, y=y, button="left", clickCount=1)

    async def key(self, k):
        codes = {"Enter": (13, "\r"), "Escape": (27, "")}
        vk, txt = codes[k] if k in codes else (ord(k.upper()), k)
        await self.cmd("Input.dispatchKeyEvent", type="keyDown", key=k, code=k, windowsVirtualKeyCode=vk, text=txt)
        await self.cmd("Input.dispatchKeyEvent", type="keyUp", key=k, code=k, windowsVirtualKeyCode=vk)

    async def shot(self, name, fmt="png"):
        r = await self.cmd("Page.captureScreenshot", format=fmt, **({"quality": 80} if fmt == "jpeg" else {}))
        data = base64.b64decode(r["data"])
        if name:
            open(os.path.join(args.out, name), "wb").write(data)
            print("saved", name, f"{len(data) / 1024:.0f} KB")
        return data

    def stop(self):
        self.proc.terminate()
        subprocess.run(["pkill", "-f", PAT])


async def main():
    # make sure both demo cameras are running, then let them accumulate some traffic
    st = api("GET", "/api/stats")
    for setup in (1, 2):
        if not any(c["setup"] == setup for c in st["cameras"]):
            api("POST", "/api/cameras", {"setup": setup, "fps": 30})
    print(f"warming up for {args.warm}s ...")
    time.sleep(args.warm)

    b = Browser()
    await b.start(1600, 1000)
    await b.cmd("Page.navigate", url=args.url)
    await asyncio.sleep(5)

    # 1. live, two cameras
    await b.move(800, 20)
    await asyncio.sleep(1)
    await b.shot("live.png")

    # 2. focus mode with labels
    await b.move(420, 420)
    await b.key("l")
    await b.key("f")
    await asyncio.sleep(2)
    await b.shot("focus.png")
    await b.key("f")
    await b.key("l")

    # 2b. the add-camera dialog
    await b.js("openAddDialog('url'); document.querySelector('#ac-url').value = 'rtsp://user:password@192.168.1.20:554/stream1'; 1")
    await asyncio.sleep(0.6)
    await b.shot("add-camera.png")
    await b.key("Escape")
    await asyncio.sleep(0.3)

    # 3. editor (select the first zone) + alarm rules
    await b.move(420, 420)
    await b.key("e")
    await asyncio.sleep(1.2)
    r = json.loads(await b.js("JSON.stringify(document.querySelector('#ed-cv').getBoundingClientRect())"))
    z = json.loads(await b.js("JSON.stringify(ED.cfg.zones[0].points)"))
    cx = sum(p[0] for p in z) / len(z)
    cy = sum(p[1] for p in z) / len(z)
    await b.click(r["x"] + cx * r["width"], r["y"] + (cy + 0.04) * r["height"])
    await asyncio.sleep(0.6)
    await b.shot("editor.png")
    await b.js("document.querySelector('[data-p=alarms]').click(); 1")
    await asyncio.sleep(0.5)
    await b.shot("editor-alarms.png")
    await b.key("Escape")
    await asyncio.sleep(0.3)

    # 4. analytics + alarms tabs (toasts hidden so they don't cover the content)
    await b.js("document.querySelector('#toasts').style.display='none'; 1")
    await b.js("document.querySelector('#bell').click(); 1")           # the alarm inbox
    await asyncio.sleep(0.6)
    await b.shot("inbox.png")
    await b.js("document.body.click(); 1")
    await b.size(1600, 1150)
    await b.js("setTab('analytics'); 1")
    await asyncio.sleep(2.5)
    await b.shot("analytics.png")
    await b.size(1600, 1000)
    await b.js("setTab('alarms'); 1")
    await asyncio.sleep(2)
    await b.shot("alarms.png")

    # 5. hero GIF: the live view, recorded from Chrome's screencast stream (smooth, ~10 fps after resampling)
    await b.js("document.querySelector('#toasts').style.display=''; setTab('live'); 1")
    await b.size(1360, 700)
    await asyncio.sleep(2)
    await b.cmd("Page.startScreencast", format="jpeg", quality=72, maxWidth=1360, maxHeight=700, everyNthFrame=1)
    raw, t0 = [], time.time()
    while time.time() - t0 < args.gif_seconds:
        try:
            m = json.loads(await asyncio.wait_for(b.ws.recv(), 2))
        except asyncio.TimeoutError:
            continue
        if m.get("method") == "Page.screencastFrame":
            raw.append((m["params"]["metadata"]["timestamp"], m["params"]["data"]))
            b.n += 1
            await b.ws.send(json.dumps({"id": b.n, "method": "Page.screencastFrameAck", "params": {"sessionId": m["params"]["sessionId"]}}))
    await b.cmd("Page.stopScreencast")
    step, frames, nxt = 0.125, [], raw[0][0] if raw else 0
    for ts, data in raw:                                  # resample to a steady 8 fps
        if ts >= nxt:
            im = Image.open(io.BytesIO(base64.b64decode(data))).convert("RGB").crop((0, 0, 1360, 640)).resize((820, 386), Image.LANCZOS)
            frames.append(im.quantize(colors=96, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE))
            nxt += step
    path = os.path.join(args.out, "hero.gif")
    frames[0].save(path, save_all=True, append_images=frames[1:], duration=125, loop=0, optimize=True, disposal=1)
    print("saved hero.gif", f"{os.path.getsize(path) / 1e6:.1f} MB", len(frames), "frames")
    b.stop()


asyncio.run(main())
