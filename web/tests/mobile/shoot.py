"""Screenshot every route at phone width, and report tap targets that are too
small to hit. 44x44 CSS px is the long-standing floor on iOS; 24x24 is the
WCAG 2.2 AA minimum. Report against 24 so the list is actionable rather than
a wall, and count how many clear 44."""
import sys, pathlib, urllib.parse
from playwright.sync_api import sync_playwright

base = sys.argv[1]; out = pathlib.Path(sys.argv[2]); out.mkdir(parents=True, exist_ok=True)
e = urllib.parse.quote("http://synthetic:9200/sparql", safe="")
ROUTES = [("index", f"{base}/"), ("about", f"{base}/about"), ("docs", f"{base}/docs"),
          ("docs-metrics", f"{base}/docs/metrics"), ("docs-states", f"{base}/docs/states"),
          ("explore", f"{base}/explore"), ("endpoint", f"{base}/endpoint?url={e}")]
TAPS = """() => {
  const small = [], all = [];
  for (const el of document.querySelectorAll('a, button, input, select, [role=button], summary')) {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) continue;
    const label = (el.innerText || el.getAttribute('aria-label') || el.tagName).trim().slice(0, 34).replace(/\\s+/g,' ');
    all.push(1);
    if (r.height < 24 || r.width < 24)
      small.push({label, w: Math.round(r.width), h: Math.round(r.height),
                  cls: (el.className||'').toString().slice(0,34)});
  }
  const seen = new Set(); const out = [];
  for (const s of small) { const k = s.cls + s.h; if (seen.has(k)) continue; seen.add(k); out.push(s); }
  return {total: all.length, small: small.length, sample: out.slice(0, 6)};
}"""
with sync_playwright() as p:
    b = p.chromium.launch()
    page = b.new_page(viewport={"width": 375, "height": 812}, device_scale_factor=2, is_mobile=True, has_touch=True)
    for name, url in ROUTES:
        page.goto(url, wait_until="networkidle", timeout=30000)
        page.screenshot(path=str(out / f"{name}.png"), full_page=False)
        t = page.evaluate(TAPS)
        print(f"  {name:14} taps={t['total']:4}  under-24px={t['small']}")
        for s in t["sample"]:
            print(f"       {s['label'][:30]:32} {s['w']}x{s['h']}  .{s['cls']}")
    b.close()
print(f"\nscreenshots -> {out}")
