"""A microdata extractor, enough of one to hold web/templates to its markup.

WHY A LOCAL ONE. `extruct` is the obvious library and the markup in
web/templates/endpoint.html WAS verified against it while being written --
that is how the double-prefixed `isMeasurementOf` was caught. It is not a
dependency here because web/requirements.txt is what the production image
installs, and extruct pulls lxml into it to serve a test.

WHAT IT IMPLEMENTS, from the HTML microdata model:

  - `itemscope` starts an item; `itemtype` types it; `itemid` names its
    subject, and without one the item is anonymous (a blank node).
  - `itemprop` on an element inside an item adds a property to the NEAREST
    enclosing item.
  - An element with `itemscope` and NO `itemprop` is a SEPARATE top-level
    item even inside another item's subtree. That rule is the one the
    endpoint page leans on, so that a measurement can state `dqv:computedOn`
    in the graph's own direction rather than needing an inverse.
  - A value comes from `href` on `link`/`a`, `content` on `meta`, `src` on
    `img`, and otherwise the element's text.

WHAT IT DOES NOT implement: `itemref`, relative-URL resolution, language
tags, and the vocabulary-relative property names schema.org uses. The page
writes absolute IRIs for every property, which is what makes the subset
sufficient; a test that needed more would be testing a different page.
"""

from __future__ import annotations

from html.parser import HTMLParser

_VALUE_ATTR = {"link": "href", "a": "href", "meta": "content", "img": "src"}
_VOID = {"link", "meta", "img", "br", "hr", "input", "source"}


class _Item:
    def __init__(self, itemtype: str | None, itemid: str | None):
        self.type = itemtype
        self.id = itemid
        self.properties: dict[str, list[str]] = {}

    def add(self, name: str, value: str) -> None:
        self.properties.setdefault(name, []).append(value)


class _Microdata(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.items: list[_Item] = []
        self._open: list[tuple[str, _Item | None, str | None]] = []
        self._stack: list[_Item] = []
        self._text: list[tuple[_Item, str, list[str]]] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        a = {k: (v or "") for k, v in attrs}
        item = None
        if "itemscope" in a:
            item = _Item(a.get("itemtype"), a.get("itemid"))
            # A property whose value is an item nests it; otherwise it is its
            # own top-level item, which is the rule the endpoint page uses.
            if "itemprop" in a and self._stack:
                self._stack[-1].add(a["itemprop"], f"[item {a.get('itemtype')}]")
            self.items.append(item)
            self._stack.append(item)
        elif "itemprop" in a and self._stack:
            attr = _VALUE_ATTR.get(tag)
            if attr is not None:
                self._stack[-1].add(a["itemprop"], a.get(attr, ""))
            else:
                # Text content, accumulated until the element closes.
                self._text.append((self._stack[-1], a["itemprop"], []))
        if tag not in _VOID:
            self._open.append((tag, item, a.get("itemprop") if item is None else None))

    def handle_endtag(self, tag: str) -> None:
        while self._open:
            name, item, prop = self._open.pop()
            if item is not None:
                self._stack.pop()
            if prop is not None and self._text and self._text[-1][1] == prop:
                owner, key, chunks = self._text.pop()
                owner.properties.setdefault(key, []).append("".join(chunks).strip())
            if name == tag:
                break

    def handle_data(self, data: str) -> None:
        if self._text:
            self._text[-1][2].append(data)


def items(html: str) -> list[_Item]:
    """Every microdata item in `html`, top-level and nested alike."""
    parser = _Microdata()
    parser.feed(html)
    return parser.items


def one(html: str, itemtype: str) -> list[_Item]:
    """Every item of `itemtype`."""
    return [i for i in items(html) if i.type == itemtype]
