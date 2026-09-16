// Ranked matching over one endpoint's vocabulary.
//
// A direct transliteration of web/vocab_match.py -- same function names, same
// tiers, same tie-breaks. That module is the specification; this file and
// tests/fixtures/vocab_match_cases.json are what keep the two from drifting
// (see tests/test_vocab_match.py::test_the_javascript_agrees_with_python).
//
// Nothing here is fetched. The DOM half below narrows what the server already
// rendered into the page; it does not go back to the network for anything.

// camelCase boundaries, and the punctuation that separates words in an IRI.
//
// This only splits a lower-to-upper transition, so a run of capitals is one
// token with whatever follows it: `hasIRI` -> ["has", "iri"] but `IRIValue` ->
// ["irivalue"] and `HTTPRequest` -> ["httprequest"]. That is a known gap in
// vocab_match.py, not a bug to fix here -- the two are checked against one
// shared table and must agree, gap included.
const BOUNDARY = /(?<=[a-z0-9])(?=[A-Z])|[_\-./:]+/;

// Below four characters, edit distance 1 matches almost everything, so the
// fuzzy tier would return the whole vocabulary for a three-letter typo.
const MIN_FUZZY_LENGTH = 4;

/** One term's name, as the words it is written from. */
export function tokenize(name) {
  return name
    .split(BOUNDARY)
    .filter((part) => part)
    .map((part) => part.toLowerCase());
}

/** Damerau-Levenshtein distance <= 1, decided without building a matrix.
 *
 * Transposition counts as ONE edit, and that is not a refinement -- it is the
 * case this tier exists for. `recpetor` for `receptor` is two adjacent
 * letters swapped, which plain Levenshtein scores as 2, so a distance-1
 * Levenshtein test rejects the very example this feature was specified
 * around.
 */
function withinOneEdit(a, b) {
  if (a === b) return true;
  let la = a.length;
  let lb = b.length;
  if (Math.abs(la - lb) > 1) return false;
  if (la > lb) {
    [a, b] = [b, a];
    [la, lb] = [lb, la];
  }
  // Find the first and last positions where they differ.
  let head = 0;
  while (head < la && a[head] === b[head]) head += 1;
  let tail = 0;
  while (tail < la - head && a[la - 1 - tail] === b[lb - 1 - tail]) tail += 1;

  if (la === lb) {
    const middle = la - head - tail;
    if (middle <= 1) return true; // one substitution, or none
    // One adjacent transposition: exactly two differing characters, swapped.
    return middle === 2 && a[head] === b[head + 1] && a[head + 1] === b[head];
  }
  // One insertion or deletion: the differing run is a single character.
  return la - head - tail === 0;
}

/** How well one typed word answers to one term. 0 (not at all) to 4 (exact). */
export function scoreWord(word, tokens, haystack) {
  if (tokens.some((t) => t === word)) return 4;
  if (tokens.some((t) => t.startsWith(word))) return 3;
  if (haystack.includes(word)) return 2;
  if (word.length >= MIN_FUZZY_LENGTH && tokens.some((t) => withinOneEdit(word, t))) return 1;
  return 0;
}

/** The terms that answer to a query, best first, each in its band.
 *
 * A term is a `match` when every typed word scores 2 or better -- it is
 * present, even if not adjacently or in that order. It is a `close` match
 * when at least half the words score at all, which is what a typo produces.
 * Anything else is omitted rather than shown greyed: a list that never
 * shortens does not answer the question "is this here".
 */
export function rank(terms, query) {
  const words = query.trim().toLowerCase().split(/\s+/).filter((w) => w);
  if (words.length === 0) return terms.slice();

  const ranked = [];
  for (const term of terms) {
    const tokens = (term.tokens || "").split(/\s+/).filter((t) => t);
    const haystack = [term.local || "", term.prefix || "", term.iri || ""]
      .join(" ")
      .toLowerCase();
    const scores = words.map((w) => scoreWord(w, tokens, haystack));
    let band;
    if (scores.every((s) => s >= 2)) {
      band = "match";
    } else if (scores.filter((s) => s >= 1).length * 2 >= words.length) {
      band = "close";
    } else {
      continue;
    }
    ranked.push({
      ...term,
      band,
      score: scores.reduce((a, b) => a + b, 0),
    });
  }

  // Band first, then total score, then the shorter name, then alphabetically,
  // then the IRI. The first four exist so the order cannot shuffle between
  // keystrokes; they are not enough to make the order total by themselves --
  // two different terms can share a `local` (rdfs:label and skos:label are
  // both ordinary), and would then tie on every one of them, leaving the
  // result order to whatever order `terms` happened to arrive in. The IRI is
  // unique per term, so it is the tiebreak that actually finishes the job.
  ranked.sort((a, b) => {
    const bandA = a.band === "match" ? 0 : 1;
    const bandB = b.band === "match" ? 0 : 1;
    if (bandA !== bandB) return bandA - bandB;
    if (a.score !== b.score) return b.score - a.score;
    const la = (a.local || "").length;
    const lb = (b.local || "").length;
    if (la !== lb) return la - lb;
    const localA = a.local || "";
    const localB = b.local || "";
    if (localA !== localB) return localA < localB ? -1 : 1;
    const iriA = a.iri || "";
    const iriB = b.iri || "";
    if (iriA !== iriB) return iriA < iriB ? -1 : 1;
    return 0;
  });
  return ranked;
}

// The DOM half. Guarded so importing this module under node (the agreement
// test above) needs no document.
if (typeof document !== "undefined") {
  (function () {
    const q = document.getElementById("vocab-q");
    const list = document.getElementById("vocab");
    const count = document.getElementById("vocab-count");
    const none = document.getElementById("vocab-none");
    if (!q || !list || !count) return;

    const matchHeading = list.querySelector('[data-band="match"]');
    const closeHeading = list.querySelector('[data-band="close"]');

    // Read once. A term's tokens are built server-side into data-tok, so
    // typing does not re-tokenise every row's name on every keystroke. `local`
    // and `iri` come from the spans already on the row (no extra fetch,
    // nothing rebuilt); `rank` derives its own haystack from them, which is
    // the same local+prefix+iri join data-hay was built from server-side.
    const items = [].slice.call(list.children)
      .filter((el) => el.tagName === "LI")
      .map((li) => {
        const localEl = li.querySelector(".v-local");
        const prefixEl = li.querySelector(".v-prefix");
        const iriEl = li.querySelector(".v-iri");
        return {
          el: li,
          local: localEl ? localEl.textContent.trim() : "",
          prefix: prefixEl ? prefixEl.textContent.trim() : "",
          iri: iriEl ? (iriEl.getAttribute("title") || iriEl.textContent.trim()) : "",
          tokens: li.getAttribute("data-tok") || "",
        };
      });
    const total = parseInt(count.getAttribute("data-total"), 10) || items.length;

    const originalOrder = items.slice();
    const fragment = document.createDocumentFragment();

    function showServerOrder() {
      originalOrder.forEach((it) => {
        it.el.hidden = false;
        fragment.appendChild(it.el);
      });
      list.appendChild(fragment);
      if (matchHeading) matchHeading.hidden = true;
      if (closeHeading) closeHeading.hidden = true;
      count.textContent = total + " terms";
      if (none) none.hidden = true;
    }

    function apply() {
      const needle = q.value;
      if (!needle.trim()) {
        showServerOrder();
        return;
      }

      const ranked = rank(items, needle);
      const rankedEls = new Set(ranked.map((t) => t.el));

      // Hide everything not ranked, first, so nothing stale stays visible
      // while the fragment is assembled.
      originalOrder.forEach((it) => {
        if (!rankedEls.has(it.el)) it.el.hidden = true;
      });

      let sawMatch = false;
      let sawClose = false;
      ranked.forEach((t) => {
        t.el.hidden = false;
        if (t.band === "match" && !sawMatch) {
          if (matchHeading) {
            matchHeading.hidden = false;
            fragment.appendChild(matchHeading);
          }
          sawMatch = true;
        }
        if (t.band === "close" && !sawClose) {
          if (closeHeading) {
            closeHeading.hidden = false;
            fragment.appendChild(closeHeading);
          }
          sawClose = true;
        }
        fragment.appendChild(t.el);
      });
      if (matchHeading && !sawMatch) matchHeading.hidden = true;
      if (closeHeading && !sawClose) closeHeading.hidden = true;
      list.appendChild(fragment);

      const shown = ranked.length;
      count.textContent = shown + " of " + total + " terms";
      if (none) none.hidden = shown !== 0;
    }

    q.addEventListener("input", apply);
    apply();
  })();
}
