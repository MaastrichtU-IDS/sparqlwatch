"""The verdict encoding, in one place.

This module is the web tier's single implementation of
docs/design/verdict-encoding.md. That document is canonical: seven states
across three channels that are all independent of colour, namely border
style, whether the swatch is filled, and border weight. Colour still carries
meaning for a reader who can see it, but it never carries meaning alone.

The reason this is a module rather than a few CSS rules written twice is that
the JavaScript viewer (tools/render-run.mjs) had two copies before it had one,
and they drifted twice. The legend ended up giving ``absent`` a fill the chips
never had, so the one verdict that claims a negative was explained by a swatch
that did not match it, and a `dotted` style rendered as solid in one of the
two. A legend that disagrees with the thing it explains is worse than no
legend.

So there is exactly one table here, and everything the page shows is derived
from it:

  * ``css_rules()`` emits one CSS rule per state, named by ``css_class()``.
  * the chips use ``css_class(state.slug)``.
  * the legend swatches use ``css_class(state.slug)``.

Chips and swatches therefore cannot disagree: they are the same class.

The property the whole encoding rests on is that the (border, fill, weight)
triples are injective, because a reader who cannot separate the colours has
nothing else left. web/tests/test_page.py asserts that, and asserts this table
still equals the one in the canonical document.
"""

from __future__ import annotations

from dataclasses import dataclass

# The chip geometry, in pixels, and why it is fixed here rather than left to
# the stylesheet: the fill token below is calibrated to it. A fill that is
# visible on a large panel can be invisible on a chip this size, which is
# exactly the defect docs/design/verdict-encoding.md describes.
CHIP_WIDTH_PX = 26
CHIP_HEIGHT_PX = 22

# 16% white over either background. The first attempt at this reused the theme's
# --overlay token, 5% white, which is tuned for panels and vanishes at chip
# size: desaturating the page showed filled and empty chips reading identically,
# so the one channel separating "works" from "we never found out" carried
# nothing at all. Nothing about that was visible from the code.
CHIP_FILL = "rgba(255, 255, 255, 0.16)"


@dataclass(frozen=True)
class Presentation:
    """How one state is drawn, on three channels plus colour.

    ``border`` is a CSS border-style, with ``"none"`` meaning no border at all
    (drawn as a transparent one, so a chip with no border still occupies a
    chip's worth of space and the row does not shift). ``fill`` is whether the
    swatch carries the fill token. ``weight`` is the border width in pixels.
    ``token`` is a theme colour variable name, and it is the one channel that
    is allowed to be redundant.
    """

    slug: str
    label: str
    meaning: str
    border: str
    fill: bool
    weight: int
    token: str

    @property
    def triple(self) -> tuple[str, bool, int]:
        """The colour-free part of the presentation.

        Two states sharing this are indistinguishable to a reader who cannot
        separate the colours, which is what the injectivity test forbids.
        """
        return (self.border, self.fill, self.weight)


# The seven states, in the canonical document's order. Six verdicts, matching
# prober/src/verdict.rs's slugs exactly, plus one fact that is deliberately not
# a verdict: a declined metric was never measured, so it has no verdict at all,
# and "we chose not to look" is not a finding about the endpoint.
STATES: tuple[Presentation, ...] = (
    Presentation(
        slug="verified",
        label="verified",
        meaning="confirmed and declared",
        border="solid",
        fill=True,
        weight=1,
        token="good",
    ),
    Presentation(
        slug="undeclared-but-verified",
        label="confirmed, not declared",
        meaning="confirmed, not declared",
        border="dashed",
        fill=True,
        weight=1,
        token="good",
    ),
    Presentation(
        slug="declared-only",
        label="declared only",
        meaning="declared, not confirmed",
        border="solid",
        fill=False,
        weight=1,
        token="text-muted",
    ),
    Presentation(
        slug="declared-but-wrong",
        label="declared but wrong",
        meaning="declared, but incorrect",
        border="solid",
        fill=True,
        weight=2,
        token="crit",
    ),
    Presentation(
        slug="indeterminate",
        label="indeterminate",
        meaning="not determined",
        border="dashed",
        fill=False,
        weight=1,
        token="warn",
    ),
    Presentation(
        slug="absent",
        label="absent",
        meaning="neither declared nor confirmed",
        border="none",
        fill=False,
        weight=1,
        token="text-dim",
    ),
    Presentation(
        slug="not-measured",
        label="not measured",
        meaning="not measured",
        border="dotted",
        fill=False,
        weight=1,
        token="text-dim",
    ),
)

# The slug of the state a declined metric is drawn in. Named rather than
# spelled out at the call site so that the one place the page turns an
# sw:NotMeasured fact into a presentation is traceable from this table.
NOT_MEASURED = "not-measured"

# A verdict this build does not know about, from a prober newer or older than
# this page. It is NOT one of the seven: it is what we draw when the graph
# carries a value our table has no entry for.
#
# The JavaScript viewer's fallback for this case reuses `absent`'s exact
# presentation (no border, unfilled), which draws an unknown value as the one
# verdict that claims a negative: "we have no idea what this means" rendered as
# "we established that nothing was there". This uses a border style none of the
# seven uses, so an unrecognised verdict is visibly unrecognised, and it is
# still shown rather than dropped, because a page that hides a measurement is
# worse than one that admits it does not understand it.
UNRECOGNISED = Presentation(
    slug="unrecognised",
    label="unrecognised verdict",
    meaning="a value this page has no encoding for; shown, not dropped",
    border="double",
    fill=False,
    weight=3,
    token="accent",
)

_BY_SLUG = {state.slug: state for state in STATES}


def presentation(slug: str) -> Presentation:
    """The presentation for a verdict slug, or ``UNRECOGNISED``.

    Never raises and never returns None: every row the store holds has to be
    drawable, because the alternative is a page that silently omits a
    measurement it was handed.
    """
    return _BY_SLUG.get(slug, UNRECOGNISED)


def css_class(slug: str) -> str:
    """The class name that draws one state.

    One name shared by the chips and the legend swatches. They cannot drift
    apart while they are the same class, which is the whole point.
    """
    return "enc-" + (slug if slug in _BY_SLUG else UNRECOGNISED.slug)


def _declarations(state: Presentation) -> str:
    """One state's CSS declarations, derived from its three channels."""
    # "none" keeps the weight and goes transparent rather than dropping the
    # border, so a borderless chip is still chip-sized and the row of chips
    # does not reflow around it.
    style = "solid" if state.border == "none" else state.border
    colour = (
        "transparent" if state.border == "none" else f"var(--{state.token})"
    )
    fill = CHIP_FILL if state.fill else "transparent"
    return (
        f"border: {state.weight}px {style} {colour}; "
        f"background: {fill};"
    )


def text_class(slug: str) -> str:
    """The class that puts a state's own colour on TEXT rather than on a border.

    The matrix header needs it. Its state labels were rotated swatch-plus-text
    pairs until 2026-09-01, where the swatch carried the encoding and the words
    carried none of it. Written out horizontally the words are the marker, so
    they take the state's colour, and `enc-<slug>` beside this one gives them its
    line style.

    A second class rather than a `color` added to `_declarations`, because the
    chips in the listing and the cells in the grid must NOT take it: a cell shows
    a count, and a count in the warn token reads as a warning about the number.
    """
    return "enc-text-" + (slug if slug in _BY_SLUG else UNRECOGNISED.slug)


def css_rules() -> str:
    """The stylesheet fragment for every state, generated from the table.

    Emitted rather than hand-written so that adding a state, or changing one
    channel of one state, cannot leave the legend explaining the old drawing.

    Two families. `enc-<slug>` is the border and fill that draw a chip, and
    `enc-text-<slug>` is the same state's colour for text. Both are generated
    from the one table for the same reason: a state added with no text colour
    would render its label in the default ink and silently stop being marked.
    """
    lines = []
    for state in (*STATES, UNRECOGNISED):
        lines.append(
            f"  .{css_class(state.slug)} {{ {_declarations(state)} }}"
        )
    for state in (*STATES, UNRECOGNISED):
        # The token, never the border colour: `absent` draws no border, and a
        # label in `transparent` would be an invisible label.
        lines.append(
            f"  .{text_class(state.slug)} {{ color: var(--{state.token}); }}"
        )
    return "\n".join(lines)
