# Mobile layout tests

Two scripts, both needing a **running site** and a **Chromium**, which is why
neither is in the default `pytest` run: the suite is in-process and offline by
design, and these are neither.

    pip install playwright && python -m playwright install chromium

## probe_mobile.py — does anything overflow?

    python tests/mobile/probe_mobile.py http://127.0.0.1:8000

Loads all seven routes at three phone viewports and reports the horizontal
overflow per route, naming the elements responsible. Exit status is nonzero if
anything overflows, so it works as a gate.

"Responsible" is doing real work in that sentence. An element is only named if
its own right edge passes the viewport AND it is not inside a scroll container:
without the second test, every cell of a deliberately-scrolling table is
reported, and the first version of this script named eight things to fix on a
page with nothing wrong with it.

## shoot.py — what does it look like, and can it be tapped?

    python tests/mobile/shoot.py http://127.0.0.1:8000 /tmp/shots

Screenshots each route at 375px and reports tap targets under 24x24 CSS px
(WCAG 2.2 AA). Expect a handful: links inside a sentence are exempt from that
rule and are counted anyway, because deciding which ones are prose is a
judgement this script should not make on its own. The header nav is not prose
and is held to the floor.

**Look at the screenshots.** Overflow being zero is not the same as the page
being usable, and everything worth fixing on the first pass here was found by
looking rather than by measuring: a header wrapping to three ragged lines, and
a fleet grid rendering its column headers above an empty body.
