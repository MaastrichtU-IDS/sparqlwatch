# The verdict encoding, and why colour is never load-bearing

**This document is the canonical encoding.** The design artboards in `design/`
predate it and still carry an earlier version; where they disagree, this wins.
The working implementation is `tools/render-run.mjs`.

## The seven states

Six verdicts, plus one fact that is deliberately not a verdict.

| state | border | fill | weight | means |
|---|---|---|---|---|
| `verified` | solid | filled | 1px | works, and the endpoint declares it |
| `undeclared-but-verified` | dashed | filled | 1px | works, no declaration seen |
| `declared-only` | solid | empty | 1px | claimed, not confirmable by probe |
| `declared-but-wrong` | solid | filled | **2px** | answered, and answered incorrectly |
| `indeterminate` | dashed | empty | 1px | we never got to find out |
| `absent` | none | empty | 1px | neither claimed nor observed |
| `not measured` | dotted | empty | 1px | we declined to look (cost ceiling) |

The channels carry meaning rather than being arbitrary:

- **filled** means we have positive evidence the capability works
- **dashed** means no declaration was seen for it
- **2px** means something is actively wrong rather than merely missing
- **dotted** means we did not look
- **no border** means nothing was there

## Why three channels and not one

An earlier encoding used border style alone, and it had **two** collisions:

- `solid` covered `verified`, `declared-only` and `declared-but-wrong`
- `dashed` covered `undeclared-but-verified` and `indeterminate`

So a reader who cannot separate green from amber could not tell "works, just
undeclared" from "we never found out". Worse, a reader who cannot separate grey
from red could not tell `declared-only`, which is neutral, from
`declared-but-wrong`, which is the worst verdict in the vocabulary. Seven states
do not fit in four border styles.

## How to check a change to this

**Desaturate the rendered page and look at it.** Do not reason about whether the
encoding survives without colour; render it and remove the colour.

That is not pedantry, it is how the current encoding was fixed twice. The first
attempt reused the existing `--overlay` token for the fill, which is 5% white,
tuned for large panels, and completely invisible on a 26 by 22 pixel chip: filled
and empty chips read identically in greyscale, so the channel separating "works"
from "we never found out" carried nothing at all. A dedicated 16% token survives
desaturation. Nothing about that was visible from the code.

On macOS:

```sh
node tools/render-run.mjs run.nq page.html
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless \
  --window-size=1100,580 --screenshot=page.png "file://$PWD/page.html"
sips -m "/System/Library/ColorSync/Profiles/Generic Gray Gamma 2.2 Profile.icc" \
  page.png --out page-gray.png
```

## One place, not two

`chipStyle` builds the border and fill for both the chips and the legend
swatches. They had their own copies before, and had already drifted: the legend
gave `absent` a fill the chips never had, so the one verdict that claims a
negative was explained by a swatch that did not match it. A legend that disagrees
with the thing it explains is worse than no legend.

## What the web tier inherits

Stage 3 renders these states on every screen, so this encoding is the thing to
build from rather than the artboards. The artboards remain the authority for
layout, typography, spacing and the theme tokens; they are not the authority for
verdict encoding.
