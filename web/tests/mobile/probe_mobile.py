"""Measure what overflows at phone width, on the real pages.

Run against a live site:  probe_mobile.py https://host
or against a local store:  probe_mobile.py            (starts uvicorn itself)

Reports, per route per viewport: the horizontal overflow in px and the
elements responsible. An element is "responsible" when its own right edge
exceeds the viewport -- a parent that merely contains an overflowing child
is not the cause and is filtered out, so the list names things to fix
rather than the whole ancestor chain.
"""
import sys, json
from playwright.sync_api import sync_playwright

VIEWPORTS = [("iphone-se", 375, 667), ("pixel-7", 412, 915), ("iphone-14-pro-max", 430, 932)]

def routes(base, endpoint_url):
    import urllib.parse
    e = urllib.parse.quote(endpoint_url, safe="")
    return [("/", f"{base}/"), ("/about", f"{base}/about"), ("/docs", f"{base}/docs"),
            ("/docs/metrics", f"{base}/docs/metrics"), ("/docs/states", f"{base}/docs/states"),
            ("/explore", f"{base}/explore"), ("/endpoint", f"{base}/endpoint?url={e}")]

PROBE = """() => {
  const vw = document.documentElement.clientWidth;
  const doc = document.documentElement.scrollWidth;
  const bad = [];
  for (const el of document.querySelectorAll('body *')) {
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue;
    const over = Math.round(r.right - vw);
    if (over > 1) {
      // A scroll container that clips its own children is doing its job, and
      // NEITHER IS ITS CONTENT AN OFFENDER. Checking only the element itself
      // reported every cell of a scrolling table as overflowing the viewport,
      // which is exactly what a scrolling table is for: the report named 8
      // things to fix on a page that had nothing wrong with it.
      let inScroller = false;
      for (let a = el; a && a !== document.body; a = a.parentElement) {
        const s = getComputedStyle(a);
        if (s.overflowX === 'auto' || s.overflowX === 'scroll') { inScroller = true; break; }
      }
      if (inScroller) continue;
      bad.push({tag: el.tagName.toLowerCase(),
                cls: (el.className && el.className.toString().slice(0,60)) || '',
                id: el.id || '', over, w: Math.round(r.width)});
    }
  }
  // Only the widest few, deduped by class+tag.
  const seen = new Set(); const out = [];
  bad.sort((a,b) => b.over - a.over);
  for (const b of bad) { const k = b.tag + '.' + b.cls; if (seen.has(k)) continue; seen.add(k); out.push(b); }
  return {vw, doc, overflow: doc - vw, offenders: out.slice(0, 8)};
}"""

def main():
    base = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8000"
    endpoint_url = sys.argv[2] if len(sys.argv) > 2 else "http://synthetic:9200/sparql"
    worst = 0
    with sync_playwright() as p:
        b = p.chromium.launch()
        for name, w, h in VIEWPORTS:
            page = b.new_page(viewport={"width": w, "height": h}, device_scale_factor=2, is_mobile=True, has_touch=True)
            print(f"\n=== {name}  {w}x{h} ===")
            for label, url in routes(base, endpoint_url):
                try:
                    page.goto(url, wait_until="networkidle", timeout=30000)
                except Exception as exc:
                    print(f"  {label:16} ERROR {exc}"); continue
                r = page.evaluate(PROBE)
                worst = max(worst, r["overflow"])
                flag = "OVERFLOW" if r["overflow"] > 1 else "ok"
                print(f"  {label:16} {flag:8} doc={r['doc']}px vw={r['vw']}px  +{r['overflow']}px")
                for o in r["offenders"]:
                    print(f"       {o['tag']}.{o['cls'][:44]:46} w={o['w']:5} over=+{o['over']}")
            page.close()
        b.close()
    print(f"\nworst horizontal overflow: {worst}px")
    return 1 if worst > 1 else 0

if __name__ == "__main__":
    sys.exit(main())
