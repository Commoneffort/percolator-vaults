"""Records the demo video scenes from the live app with Playwright (system Python).

Each scene lasts at least as long as its narration (build/audio/durations.json). The app is
driven with a funded Devnet Burner wallet whose key is injected into localStorage.
"""
import json
import sys
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

APP = "https://percolator-vaults.vercel.app"
HERE = Path(__file__).resolve().parent
BUILD = HERE / "build"
DUR = json.loads((BUILD / "audio" / "durations.json").read_text())
KEY = json.loads((HERE.parent / "keys" / "seed" / "demo-video.json").read_text())
MARKETS = json.loads((BUILD / "markets.json").read_text())  # {"open": sym, "btc": vault}

CURSOR_JS = """
(() => {
  const add = () => {
    if (document.getElementById('__cursor')) return;
    const c = document.createElement('div');
    c.id = '__cursor';
    c.style.cssText = 'position:fixed;left:640px;top:360px;width:22px;height:22px;margin:-11px 0 0 -11px;border-radius:50%;' +
      'background:rgba(124,156,255,.35);border:2px solid #7c9cff;z-index:2147483647;pointer-events:none;transition:transform .12s';
    document.body.appendChild(c);
    addEventListener('mousemove', e => { c.style.left = e.clientX + 'px'; c.style.top = e.clientY + 'px'; }, true);
    addEventListener('mousedown', () => c.style.transform = 'scale(.6)', true);
    addEventListener('mouseup', () => c.style.transform = 'scale(1)', true);
  };
  if (document.body) add(); else addEventListener('DOMContentLoaded', add);
})();
"""


def burner_js(auto_connect: bool) -> str:
    js = f"localStorage.setItem('percolator-vaults:devnet-burner', '{json.dumps(KEY)}');"
    js += "localStorage.setItem('walletName', JSON.stringify('Devnet Burner'));" if auto_connect else "localStorage.removeItem('walletName');"
    return js


class Scene:
    def __init__(self, p, name, auto_connect=True):
        self.name, self.t0 = name, None
        self.browser = p.chromium.launch()
        self.ctx = self.browser.new_context(viewport={"width": 1280, "height": 720}, record_video_dir=str(BUILD / "raw"),
                                            record_video_size={"width": 1280, "height": 720})
        self.ctx.add_init_script(burner_js(auto_connect))
        self.ctx.add_init_script(CURSOR_JS)
        self.page = self.ctx.new_page()
        self.mouse = [640, 360]

    def start(self):
        self.t0 = time.time()

    def move_to(self, loc):
        loc.scroll_into_view_if_needed()
        b = loc.bounding_box()
        x, y = b["x"] + b["width"] / 2, b["y"] + b["height"] / 2
        self.page.mouse.move(x, y, steps=25)
        self.mouse = [x, y]
        self.page.wait_for_timeout(250)

    def click(self, loc):
        self.move_to(loc)
        loc.click()
        self.page.wait_for_timeout(400)

    def type(self, loc, text):
        self.click(loc)
        loc.fill("")
        loc.type(text, delay=80)

    def scroll(self, px, ms=2500):
        steps = max(1, ms // 40)
        for _ in range(steps):
            self.page.mouse.wheel(0, px / steps)
            self.page.wait_for_timeout(40)

    def finish(self):
        # Hold the last frame until the narration is done, plus a breath.
        left = DUR[self.name] + 0.8 - (time.time() - self.t0)
        if left > 0:
            self.page.wait_for_timeout(int(left * 1000))
        video = self.page.video
        self.ctx.close()
        path = Path(video.path())
        target = BUILD / "raw" / f"{self.name}.webm"
        path.replace(target)
        (BUILD / "raw" / f"{self.name}.start").write_text(str(self.lead))
        self.browser.close()


def run(p, name, fn, url, auto_connect=True, settle=3500):
    s = Scene(p, name, auto_connect)
    t = time.time()
    s.page.goto(url, wait_until="domcontentloaded")
    s.page.wait_for_timeout(settle)
    s.lead = round(time.time() - t, 2)  # seconds of page load to trim from the start
    s.start()
    fn(s)
    s.finish()
    print(f"{name:12s} recorded")


def title(s):
    pass


def markets(s):
    s.page.wait_for_timeout(1500)
    s.scroll(500, 3000)
    s.page.wait_for_timeout(1500)
    s.scroll(500, 3000)
    s.page.wait_for_timeout(1500)
    s.scroll(-1000, 2000)


def open_market(s):
    pg = s.page
    s.click(pg.get_by_role("button", name="Select Wallet"))
    s.click(pg.locator(".wallet-adapter-modal-list li", has_text="Devnet Burner").first)
    pg.wait_for_timeout(1200)
    s.click(pg.get_by_role("button", name="Get test USDC").first)
    pg.wait_for_selector("text=Sent", timeout=30000)
    pg.wait_for_timeout(1500)
    s.click(pg.get_by_role("button", name=f"Open {MARKETS['open']}-PERP"))
    pg.wait_for_url("**/#/m/**", timeout=120000)
    pg.wait_for_timeout(4000)


def trade(s):
    pg = s.page
    pg.wait_for_timeout(1500)
    if pg.get_by_role("button", name="Create trading account").count():
        s.click(pg.get_by_role("button", name="Create trading account"))
        pg.wait_for_selector("text=Add margin", timeout=60000)
        pg.wait_for_timeout(1500)
    s.type(pg.locator("label.field:has-text('USDC') input").first, "150")
    s.click(pg.get_by_role("button", name="Add margin"))
    pg.wait_for_selector("text=Add margin: confirmed", timeout=60000)
    s.type(pg.locator("label.field:has-text('Size') input"), "0.01")
    s.click(pg.get_by_role("button", name="Long", exact=True))
    pg.wait_for_selector("text=/Long 0.01 BTC: confirmed|Long 0.01 BTC failed/", timeout=60000)
    pg.wait_for_timeout(2500)
    s.scroll(700, 2500)
    pg.wait_for_timeout(1500)


def liquidity(s):
    pg = s.page
    pg.wait_for_timeout(1000)
    s.click(pg.get_by_role("button", name="Provide liquidity"))
    s.type(pg.locator("label.field:has-text('Deposit USDC') input"), "200")
    s.click(pg.get_by_role("button", name="Request deposit"))
    pg.wait_for_selector("text=/Deposit request: confirmed|Deposit request failed/", timeout=60000)
    pg.wait_for_timeout(2500)


def leaderboard(s):
    s.page.wait_for_selector("table.lb", timeout=60000)
    s.page.wait_for_timeout(1500)


def docs(s):
    for _ in range(4):
        s.page.wait_for_timeout(1200)
        s.scroll(650, 2200)


if __name__ == "__main__":
    only = set(sys.argv[1:])
    btc = f"{APP}/#/m/{MARKETS['btc']}"
    scenes = [
        ("title", title, f"file://{HERE}/card.html?k=title", False),
        ("markets", markets, f"{APP}/#/", False),
        ("open", open_market, f"{APP}/#/launch/{MARKETS['open']}", False),
        ("trade", trade, btc, True),
        ("liquidity", liquidity, btc, True),
        ("leaderboard", leaderboard, f"{APP}/#/leaderboard", True),
        ("docs", docs, f"{APP}/#/docs", False),
        ("outro", title, f"file://{HERE}/card.html?k=outro", False),
    ]
    (BUILD / "raw").mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p:
        for name, fn, url, auto in scenes:
            if only and name not in only:
                continue
            run(p, name, fn, url, auto)
