"""Geometry for the endpoint page's two daily charts.

Numbers, not markup. This module turns the day series into coordinates and the
template draws them. The split is what makes the charts testable: a bar drawn at
the wrong height is a wrong number here, which an assertion can catch, and that
is not true of an f-string in a page.

THE TWO CHARTS SHARE AN X-AXIS AND NEVER A Y-AXIS. Uptime is a share and
response time is a duration; putting two scales in one frame invites reading
the crossing of two lines as an event, when it only records the two scales
chosen. They stack instead -- day i at the same x in both, one set of date
labels under the pair -- so a reader still carries a date from one to the other.

One viewBox, no resize script: the SVG is authored at VIEW_W and scaled by CSS
to whatever width it gets. Geometry computed once on the server survives every
viewport, which is why none of this needs to run again in the browser.
"""

import math

# The frame. VIEW_W is a coordinate space, not a pixel count -- the page scales
# the drawing to its column -- so these are ratios in disguise and only their
# proportions matter.
VIEW_W = 720
PAD_L = 40  # room for a y label of "100" / "1500"
PAD_R = 8

UPTIME_H = 100  # plot height, uptime
RESPONSE_H = 90  # plot height, response time
PLOT_TOP = 8  # both plots, below their own top edge
LABEL_ROW = 20  # the shared date strip under the lower chart

# A bar carries its value in its LENGTH, so its axis starts at zero and the
# uptime axis is pinned 0-100 besides. A 98-100 zoom is the standard way to make
# an ordinary week look like a cliff, and this page is read by people deciding
# whether to depend on a server.
UPTIME_TICKS = (0, 50, 100)

# At most this many dates are drawn under the pair. Thirty labels at this width
# overlap into a grey smear; every fifth is a scale a reader can still use.
MAX_DATE_LABELS = 7


def _nice_ceiling(value: int) -> int:
    """The smallest round number at or above `value`.

    An axis topped at the largest observation reads as though that observation
    were the limit of something. A round ceiling above it reads as a scale.
    """
    if value <= 0:
        return 1
    base = 10 ** math.floor(math.log10(value))
    for step in (1, 1.5, 2, 2.5, 3, 4, 5, 7.5, 10):
        if step * base >= value:
            return int(step * base)
    return int(10 * base)


def daily_charts(days: list[dict]) -> dict | None:
    """Coordinates for both charts, or None when there is nothing to draw.

    `days` is _daily_series' output: one dict per day, `uptime` None on a day
    nobody asked. A day with no measurement is a HOLE, never a zero -- it gets
    its slot and its hover target, and no mark. Drawing it as 0% would report
    an outage this service never observed.
    """
    if not days:
        return None
    plot_w = VIEW_W - PAD_L - PAD_R
    slot = plot_w / len(days)

    # The response axis is scaled to p95, not to p50: a ceiling chosen from the
    # median puts every tail above the frame.
    peak = max((d["p95_ms"] for d in days if d.get("p95_ms") is not None), default=0)
    ceiling = _nice_ceiling(peak)

    def centre(i: int) -> float:
        return PAD_L + i * slot + slot / 2

    def response_y(ms: float) -> float:
        return PLOT_TOP + RESPONSE_H - (ms / ceiling) * RESPONSE_H

    bars, slots, points, gaps = [], [], [], []
    for i, day in enumerate(days):
        # The hit target is the full-height slot, not the mark: pointing at a
        # day with 2% uptime must not require hitting a two-pixel stub, and a
        # day with no mark at all must still be answerable.
        slots.append({
            "x": round(PAD_L + i * slot, 2),
            "w": round(slot, 2),
            "day": day["day"],
            "index": i,
            "readout": _readout(day),
        })
        if day["uptime"] is None:
            # A GAP IS DRAWN, not merely left out. Absence and nothing-rendered
            # look the same, and the thing a zero-height bar looks like is a
            # gap -- so the day nobody asked gets the same dim baseline dot the
            # history matrix uses for a sweep that recorded nothing, and the day
            # that answered nothing gets a stub (below). Two marks, two states.
            gaps.append({"x": round(centre(i), 2), "day": day["day"]})
            continue
        height = (day["uptime"] / 100) * UPTIME_H
        bars.append({
            # A 2px surface gap between neighbours, so adjacent days read as two
            # bars rather than one ribbon.
            "x": round(PAD_L + i * slot + 1, 2),
            "w": round(max(slot - 2, 1), 2),
            # A FLOOR OF 2, so 0% is a mark rather than nothing. Zero length
            # is the honest encoding of zero and it is also invisible; the stub
            # says "measured, and the answer was none" where blank cannot.
            "y": round(PLOT_TOP + UPTIME_H - max(height, 2), 2),
            "h": round(max(height, 2), 2),
            "day": day["day"],
            "index": i,
        })
        if day["median_ms"] is not None:
            points.append({
                "index": i,
                "x": round(centre(i), 2),
                "p50": round(response_y(day["median_ms"]), 2),
                "p95": round(response_y(day["p95_ms"]), 2),
            })

    # AN ENDPOINT THAT NEVER ANSWERED HAS NO RESPONSE CHART. This fleet holds
    # plenty of them -- dormant, unreachable, or down for the whole window --
    # and for those `peak` is 0, which would scale a frame to a 1 ms ceiling and
    # draw an empty plot under two gridlines both labelled 0. An empty frame
    # asserts that there was something to plot. The uptime chart alone says the
    # true thing, so the date strip moves under it when that happens.
    has_response = bool(points)
    dates = _date_labels(days, centre)

    return {
        "view_w": VIEW_W,
        "has_response": has_response,
        "uptime_h": PLOT_TOP + UPTIME_H + PLOT_TOP + (0 if has_response else LABEL_ROW),
        "uptime_date_y": PLOT_TOP + UPTIME_H + 16,
        "response_h": PLOT_TOP + RESPONSE_H + PLOT_TOP + LABEL_ROW,
        "bars": bars,
        "gaps": gaps,
        "slots": slots,
        "uptime_ticks": [
            {"value": v, "y": round(PLOT_TOP + UPTIME_H - (v / 100) * UPTIME_H, 2)}
            for v in UPTIME_TICKS
        ],
        # Deduplicated: a small ceiling can make the midpoint collide with the
        # floor, and two gridlines labelled 0 read as a drawing error.
        "response_ticks": [
            {"value": v, "y": round(response_y(v), 2)}
            for v in sorted({0, ceiling // 2, ceiling})
        ],
        "bands": _segments(points, band=True),
        "lines": _segments(points, band=False),
        # A lone day between two gaps cannot be a line. It is drawn as a point,
        # because dropping it would hide a measurement that exists.
        "dots": _orphans(points),
        "dates": dates,
        "ceiling": ceiling,
        "x_left": PAD_L,
        "x_right": VIEW_W - PAD_R,
        # Both baselines named rather than recomputed in the template. The
        # page should not be doing arithmetic that has to agree with this file.
        "uptime_baseline": PLOT_TOP + UPTIME_H,
        "response_baseline": PLOT_TOP + RESPONSE_H,
        "tick_x": PAD_L - 6,
        "date_y": PLOT_TOP + RESPONSE_H + 16,
    }


def _runs(points: list[dict]) -> list[list[dict]]:
    """Consecutive days, split wherever a day is missing.

    THE SPLIT IS THE POINT. A line drawn straight across a week nobody measured
    asserts a week of measurements; breaking it says only what was seen.
    """
    runs: list[list[dict]] = []
    for point in points:
        if runs and point["index"] == runs[-1][-1]["index"] + 1:
            runs[-1].append(point)
        else:
            runs.append([point])
    return runs


def _segments(points: list[dict], band: bool) -> list[str]:
    paths = []
    for run in _runs(points):
        if len(run) < 2:
            continue
        if band:
            top = " ".join(f"{'M' if i == 0 else 'L'}{p['x']} {p['p95']}" for i, p in enumerate(run))
            back = " ".join(f"L{p['x']} {p['p50']}" for p in reversed(run))
            paths.append(f"{top} {back} Z")
        else:
            paths.append(" ".join(
                f"{'M' if i == 0 else 'L'}{p['x']} {p['p50']}" for i, p in enumerate(run)
            ))
    return paths


def _orphans(points: list[dict]) -> list[dict]:
    return [run[0] for run in _runs(points) if len(run) == 1]


def _date_labels(days: list[dict], centre) -> list[dict]:
    step = max(1, math.ceil(len(days) / MAX_DATE_LABELS))
    # Anchored on the LAST day and counted backwards: the newest day is the one
    # a reader looks for, and it is the one an every-nth-from-the-left rule
    # leaves unlabelled.
    keep = set(range(len(days) - 1, -1, -step))
    return [
        {"x": round(centre(i), 2), "text": days[i]["day"][5:]}
        for i in sorted(keep)
    ]


def _readout(day: dict) -> str:
    """The sentence shown when a day is pointed at.

    Built here rather than in script so the markup carries it: the charts stay
    readable with no JavaScript, and the same words are what a test asserts.
    """
    if day["uptime"] is None:
        return f"{day['day']} — not measured"
    sweeps = day.get("sweeps", 0)
    sentence = f"{day['day']} — {day['uptime']:g}% of {sweeps} sweep{'s' if sweeps != 1 else ''}"
    if day["median_ms"] is not None:
        sentence += f", {day['median_ms']} ms median, {day['p95_ms']} ms p95"
    return sentence
