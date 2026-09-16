# Website redesign: the decisions taken during execution

This is the execution ledger of the 11-task website redesign, merged 2026-09-16.
It is kept because it is the only record of 28 rulings made on the owner's behalf
while the plan ran -- including ten corrections to the plan and the spec, each
recorded with what it would cost if the correction were wrong.

Its chief use to a later reader is the pattern in those corrections: four separate
tests were found to pass whether or not the property they named held, and two
"measured facts" turned out to hold only for the fixture they were measured on.
Both failure modes are cheap to reproduce and were only ever caught by running the
thing rather than reading it.

Spec: `docs/superpowers/specs/2026-09-15-website-redesign-design.md`
Plan: `docs/superpowers/plans/2026-09-15-website-redesign.md`

---

# SDD ledger — plan: docs/superpowers/plans/2026-09-15-website-redesign.md

Spec: docs/superpowers/specs/2026-09-15-website-redesign-design.md (revision 2) — reachable.
Worktree: .worktrees/website-redesign, branch website-redesign, from c8fe2d5.

## Pre-flight scan

### Shared files / interfaces, pairwise

| tasks | shared | produced vs consumed | finding |
|---|---|---|---|
| 1 → 3 | app.py, base.html | T1 produces STYLESHEET_PATH + `stylesheet_path` global; T3's base.html links it | clean |
| 1 → 2 | static/site.css, verdict_encoding.py | T1 declares `--fill` both surfaces; T2 emits `var(--fill)` | **CONFLICT — see Ruling 3** |
| 3 → 4,5,8 | base.html | T3 produces blocks title/content/head_extra + `nav`; all three extend | clean |
| 3 → 5,6 | tests/test_index.py | T3 fixes the pinned nav assertion; T5/T6 add tests | clean, T3 must land first |
| 3 → 4,10 | tests/test_explore.py | T3 fixes pinned nav; T4 adds stylesheet test; T10 borrows endpoint const | clean |
| 5 → 8,10 | templates/endpoint.html | T5 deletes `{{ encoding_css }}`; T8 restructures; T10 adds data-tok | **GAP — see Ruling 4** |
| 5 → 6 | index.html, app.py | T5 produces `summary`; T6 adds `matching`, renames nothing | clean |
| 6 → 7 | app.py | T6 produces `_matches_query`; T7 applies it to RDF subjects | clean |
| 6 → 7 | tests | T6 defines `rows_of` in test_index.py; T7's test uses it in test_negotiation.py | **GAP — see Ruling 5** |
| 9 → 10 | vocab_match.py | T9 produces tokenize/score_word/rank; T10 imports tokenize, transliterates rank | clean |
| 1,3,4,5,8,11 | static/site.css | append-only, each task moves its own rules in | clean — order is 1,3,4,5,8,11 |

### Per-task self-consistency

| task | its tests vs its code | finding |
|---|---|---|
| 1 | 4 tests vs route + token file | clean |
| 2 | fill assertions vs the token Task 1 wrote | **CONFLICT — Ruling 3** |
| 3 | nav + no-inline-token vs base.html + _nav_context | clean |
| 4 | stylesheet test vs conversion; focus-visible replaces `outline:none` | clean |
| 5 | 4 figures vs summary dict; types corrected in self-review | clean |
| 6 | 3 tests vs `q` param + denominator | clean |
| 7 | negotiation agreement vs post-CONSTRUCT filter | clean |
| 8 | six sections + rail vs layout change, no new context keys | clean |
| 9 | scoring table vs `_within_one_edit` | **CONFLICT — Ruling 1**; fixture vs band rule — **Ruling 2** |
| 10 | agreement + tokens vs JS + explore_payload | clean |
| 11 | probe + doc vs consolidation | clean |

## Rulings

Ruling 1 (load-bearing, Task 9): The headline example `recpetor` → `Receptor` is an
adjacent TRANSPOSITION, which is Levenshtein distance 2, not 1. Verified: plain
Levenshtein<=1 returns False for that pair, so the plan's own test case would fail
against the plan's own implementation. Spec §5 says "Levenshtein distance 1" but the
example it gives — and the screen the owner approved — require Damerau-Levenshtein
(one adjacent transposition counts as one edit). The example is the approved artefact;
the metric name was imprecise prose. DECIDED: use Damerau-Levenshtein. Plan Task 9 and
spec §5 amended. COST IF WRONG: a slightly wider fuzzy tier — one transposition now
matches where it did not. Reversible by one predicate.

Ruling 2 (Task 9): The fixture's "drug target" case expects Pathway omitted, but the
spec's band rule ("close" when at least half the words score >= 1) includes it: Pathway
scores 2 on "drug" because the haystack carries the `drugbank` prefix. Verified by
running the rules. The spec's band rule is binding, and the approved screen showed
partial matches under a "some words matched" heading, so namespace matches belong there.
DECIDED: the fixture is wrong, not the rule. Expected becomes
["DrugTarget","targetOfDrug","Target","Pathway"]. COST IF WRONG: searches in a
single-namespace endpoint show more close matches than wanted; tightening the band later
is a one-line change and a fixture edit.

Ruling 3 (Task 2): Task 2's test asserts `"rgba(255, 255, 255, 0.16)" not in body`, but
Task 1 writes exactly that string as the DARK `--fill` declaration. The assertion is
self-defeating and would fail after a correct fix. DECIDED: scope the assertion to the
generated rules — assert `"background: rgba("` is absent, which catches a literal in
`_declarations` without touching the token declaration it must not catch. COST IF WRONG:
none; it tests the same property more precisely.

Ruling 4 (Task 5, minor): Task 5 Step 3 edits `templates/endpoint.html:193` but Task 5's
Files block lists only index.html. DECIDED: add endpoint.html to Task 5's Files.
COST IF WRONG: none.

Ruling 5 (Task 7, minor): Task 7's test calls `rows_of`, which Task 6 defines in
test_index.py. DECIDED: Task 7 defines its own local helper in test_negotiation.py rather
than importing across test modules, matching how this repo's test files already carry
their own fixtures and helpers. COST IF WRONG: a duplicated four-line helper.

## Progress

Task 1: dispatched (base 978357a, sonnet) — stylesheet route + token file + test_static.py
Task 1: implemented (commit 29e4560), tests 4/4, suite 447 passed / 1 pre-existing skip.
Task 1: controller check — all 24 token values verified character-exact against Global
  Constraints, both surfaces; `color-scheme: light dark` confirmed at site.css:8.
Task 1: review dispatched (sonnet, package review-978357a..29e4560.diff)
Task 1: review clean — spec ✅, quality approved, no Critical/Important.
Task 1: ⚠️ "could not re-run tests" RESOLVED by controller — the worktree has no venv of
  its own; ran with /Users/micheldumontier/code/sparqlwatch/web/.venv/bin/python -m pytest
  → tests/test_static.py 4 passed. Every later dispatch carries that command.
Task 1: complete (commits 978357a..29e4560, review clean)
Task 2: dispatched (base 29e4560, haiku) — CHIP_FILL becomes var(--fill)
Task 2: implemented (commit 24a72e3), test_static 6/6, test_page 86/86, suite 449 passed/1 skip.
Task 2: controller check — served rules now read `background: var(--fill)` for filled states
  and `transparent` for declared-only; no colour literal survives in the generated CSS.
Task 2: review dispatched (sonnet, package review-29e4560..24a72e3.diff)
Task 2: review clean — spec ✅, quality approved. Reviewer hand-reverted CHIP_FILL to prove
  the regression test fires, then restored; independently recomputed the WCAG arithmetic
  (1.604 / 1.629 / 1.136) and confirmed the 1.4 threshold sits between vanish and work.
Task 2: minor (deferred): test_the_generated_rules_read_the_fill_token's negative assertion
  matches only the literal "background: rgba(" — a differently-formatted literal (e.g.
  #00000033) in ONE filled state would pass both assertions, since the other two states
  keep var(--fill). Tighter test: assert all three fill=True states emit var(--fill).
  Not looped per the Minor rule; final review to triage.
Task 2: complete (commits 29e4560..24a72e3, review clean)

Ruling 6 (Task 3, found by controller before dispatch): the plan's base.html footer renders
  {{ void_path }}, but void_path is defined in only two contexts (app.py:1218 _page_context,
  app.py:2753 docs-void) while index_path/docs_path are in four. Jinja's default Undefined
  renders empty, so six of eight pages would have shipped href="" with no error anywhere.
  DECIDED: _nav_context() supplies index_path, docs_path AND void_path, so every page that
  extends base.html has all three; the existing per-context copies stay (removing them is
  churn outside this task). COST IF WRONG: three redundant dict keys.
Task 3: dispatched (base 24a72e3, sonnet) — base.html + four docs pages + nav table

Ruling 7 (Task 5, found by controller while Task 3 ran): the plan renders the freshness
  figure as {{ summary.last_sweep }}, but FleetStats.last_sweep (fleet.py:80,134) is an ISO
  timestamp string — index.html:491 already slices it `[:10]` to get a date. Rendered raw it
  would put "2026-09-15T03:30:12Z" in a slot the mockup showed as "41m". There is no
  relative-time helper in this codebase to reuse (index.html:400 prints newest_generated_at
  raw). DECIDED: the figure renders `summary.last_sweep[:10]` as a date, guarded by
  {% if summary.last_sweep %} because the field is `str | None`, and the label reads
  "last sweep". COST IF WRONG: a date where a duration would read better; cosmetic, one line.

Prepared context for later dispatches (verified, not assumed):
  - Registry rows are `<li data-endpoint="{{ row.endpoint }}">` (index.html:671). Task 6's
    `rows_of` is [e["data-endpoint"] for e in with_attribute(body, "data-endpoint")] — the
    existing helper in tests/test_page.py:144, no new parser needed.
  - `row.dormant` (index.html:671, set at app.py:2052 from
    entry.newest_sweep_declined_to_ask_this_endpoint) confirms Task 5's liveness source.
Task 3: implemented (commit c1e1c69), test_docs 15/15, suite 452 passed/1 skip.
  Status DONE_WITH_CONCERNS — one scope refusal, adjudicated below.

Ruling 8 (Task 3, MY PLAN WAS WRONG): Task 3 Step 6 told the implementer to update the nav
  assertions in test_explore.py and test_index.py to the four-item list. But explore.html and
  index.html do not extend base.html until Tasks 4 and 5 — they still render their own
  two-link headers — so asserting four items now would fail against pages that genuinely
  render two. The implementer refused and was RIGHT. DECIDED: the nav-test updates move to
  the task that converts each page — test_explore.py:1263 to Task 4, test_index.py's to
  Task 5. Their intent (never one nav link per row) is preserved, not loosened.
  Also: the implementer corrected my line pointer — the exact-list assertion is
  test_index.py:1263, not :1266; :1266 is an occurrence-count test already nav-length
  agnostic. Verified both. COST IF WRONG: none; this is strictly more correct ordering.

Ruling 9 (Task 3, controller-initiated): the implementer loosened tests/test_page.py:2235
  from `'<footer class="site-foot">' in body` to `"<footer" in body`, because base.html's
  footer carries no class. Loosening an existing assertion to accommodate new markup erodes
  a contract that was there for a reason. DECIDED: restore the strict assertion and give
  base.html's footer `class="site-foot"` instead — the markup adapts to the test, not the
  reverse. COST IF WRONG: one class attribute that no CSS needs.

Ruling 10 (deferred to Task 11): docs-void.html's <title> reads "States" — a pre-existing
  copy/paste slip, correctly left alone by the implementer under the no-copy-changes rule.
  A page whose tab says "States" when it is the VoID page is a real defect, and a <title>
  is not a claim about an endpoint. DECIDED: fix in Task 11 as a one-word correction.
  COST IF WRONG: none.

Task 3: environment check — playwright IS available in the main venv, so Task 11's mobile
  probe can run. The Task 3 implementer had no headless browser and verified structurally
  via curl instead; noted so Task 11 is not planned around a missing tool.
Task 3: fix round 1/5 dispatched (Ruling 9 only)
Task 3: fix round 1/5 (1 addressed, 0 open — footer class restored, strict assertion back;
  commits c1e1c69..7503a1b). CSS left as element selectors, which match a classed footer.
Task 3: task review dispatched over the FULL range 24a72e3..7503a1b (sonnet) — the task
  review had not run before the fix, so it covers both commits, not just the fix diff.
Task 3: review clean — spec ✅, quality approved. Reviewer diffed all four page bodies
  line-by-line (verbatim confirmed), enumerated every migrated selector 1:1, and traced all
  three two-**-expansion call sites to confirm the void_path TypeError fix is complete.
Task 3: minor (deferred): index_path/docs_path/void_path are now set twice in _page_context
  (app.py:1219,1268-9), _index_context (2318-9) and _docs_context (2729-30) — once via
  **_nav_context() and again literally. Harmless (same constants, later key wins) but can
  drift. One-line cleanup for the final review to triage.
Task 3: ⚠️ "visual rendering unverified" RESOLVED by controller — rendered /docs and
  /docs/states with playwright at 1200px and 390px. No horizontal overflow at any width.
  All seven verdict chips render distinguishably on the light surface; the fill channel
  survived the move, which was this redesign's biggest single risk.
Task 3: OBSERVATION FOR THE OWNER (not a defect): the measured fill rgba(0,0,0,0.20) reads
  as a heavy grey box inside the coloured border, where the approved mockup's 7% read as a
  light tint. The measurement is right (7% = 1.173:1, below the 1.139:1 that already failed
  in this codebase once) and the mockup was wrong, but the result differs visibly from what
  was signed off. Surfaced to the owner; NOT changed unilaterally. Option if they dislike it:
  tint the fill per state, which keeps the colour-independence guarantee since the channel
  is fill-vs-empty, not hue.
Task 3: complete (commits 24a72e3..7503a1b, review clean)
Task 4: dispatched (base 7503a1b, sonnet) — about + explore onto the shell, incl. Ruling 8
Task 4: implemented (commit 835a80e), about+explore 42/42, suite 454 passed/1 skip.
  Playwright: 0px horizontal overflow at 1200px and 390px on both pages; vocab grid holds.

Ruling 11 (Task 4, MY PLAN WAS WRONG): the plan's focus-visible snippet selected
  `input[type="search"]`, but explore.html's search box and endpoint.html's #vocab-q are
  both `type="text"` — verified, the templates carry 2 type="search" and 1 type="text".
  The selector would have silently left the one input whose `outline: none` I was replacing
  with no focus ring at all: an accessibility regression dressed as an accessibility fix.
  The implementer caught it and used `input:focus-visible` (by element, not type).
  DECIDED: correct, adopted, and it is the right selector for Task 8's #vocab-q too.
  COST IF WRONG: a focus ring on input types that did not ask for one — visible, trivial.

Task 4: controller note — _about_context() and the /explore route never merged
  _nav_context() at all (they set index_path/docs_path by hand and never set nav or
  void_path). Invisible until the templates stopped rendering their own headers. Implementer
  wired both; verified _nav_context() is now merged at every page-context site.
Task 4: review dispatched (sonnet, package review-7503a1b..835a80e.diff)
Task 4: review clean — spec ✅, quality approved, no Critical/Important. Reviewer rendered
  both pages at 1200/390px against a real fixture store, confirmed the focus ring actually
  paints (outline: solid rgb(11,92,173) 2px on #q), and checked the CSS classification both
  ways (nothing explore-only in site.css; nothing shared stranded in head_extra).
Task 4: minor (deferred): explore_path still passed into _about_context and the /explore
  route but referenced by neither template; and _about_context re-sets index_path/docs_path
  after **_nav_context() already supplied them. Same family as Task 3's deferred minor —
  one cleanup pass covers all of it.
Task 4: accepted judgment call — explore's probe_note span moved from the old page-specific
  header into {% block content %}, since base.html's header has no slot for page data. Text
  unchanged and its test still passes on the same string. Unavoidable given the fixed shell.
Task 4: complete (commits 7503a1b..835a80e, review clean)
Task 5: dispatched (base 835a80e, sonnet) — index.html + summary strip, Rulings 4/7/8
Task 5: implemented (commit e64bc4a), test_index 72 passed/1 skip, suite 456 passed/1 skip.
  Playwright: 0px overflow at 1200/390px (matrix scrolls in its own container); strip reads
  "3 endpoints / 2 answering / 1 not answering / 2026-08-27 last sweep".

Ruling 12 (Task 5, controller decision on a flagged regression): removing
  {{ encoding_css | safe }} from endpoint.html leaves that page's verdict chips unstyled
  until Task 8 converts it. The obvious fix — link the hashed stylesheet there now — is
  WORSE, and I verified why: endpoint.html still declares its own dark tokens
  (--bg: #0a1929) and does NOT declare --fill, which exists only in site.css. Linking
  site.css would give that page light's --fill (20% black) against a near-black background,
  so every chip would render as visibly-empty — the fill channel silently dead, which is
  exactly the defect Task 2 exists to prevent. DECIDED: accept the temporary regression. An
  obviously-unstyled page on an unmerged branch is a better failure mode than a
  plausible-looking page with a dead encoding channel. Task 8 closes it.
  COST IF WRONG: /endpoint looks broken to anyone browsing the branch before Task 8.

Ruling 13 (Task 5, MY RULING 4 WAS WRONG): I told this implementer the occurrence-count test
  test_the_index_carries_one_nav_link_and_never_one_per_row needed no change. It did — once
  index.html is on base.html, /docs appears twice (nav + the shared footer's "how we
  measure"), so `occurrences == 1` failed. The implementer fixed it by asserting the new
  exact counts (DOCS_PATH: 2, EXPLORE_PATH: 1) rather than weakening to an inequality, which
  keeps the "must not scale with rows" invariant exact. DECIDED: correct, adopted.
  COST IF WRONG: none; the assertion is stricter than an inequality would have been.

Task 5: two further tests (test_index's chips-from-one-table, test_page's generated-rules)
  now read the rules from STYLESHEET_PATH and additionally assert no private copy remains on
  the page — stricter than what they replaced.
Task 5: review dispatched (sonnet)
Task 5: review returned spec ✅ / quality approved with one Minor. Controller then found an
  IMPORTANT defect by rendering that neither implementer nor reviewer caught — see Ruling 14.
  (The reviewer had positively asserted index.html:398-400 "colors" the figures via
  enc-text-*; it does not.)

Ruling 14 (Task 5, IMPORTANT, controller-found): the strip's answering/not-answering figures
  carry enc-text-verified / enc-text-declared-but-wrong, but both compute to rgb(0,0,0).
  Cause verified: index.html head_extra declares `.strip .n { color: var(--text-bright) }`
  (specificity 0,2,0) which beats `.enc-text-verified { color: var(--good) }` (0,1,0), and
  --text-bright is #000000 on the light surface. The classes are decorative no-ops — a
  visual channel that looks wired and carries nothing, which is the exact failure class this
  redesign exists to prevent, and which no test asserts.
  The reviewer separately flagged (as Minor) that borrowing per-metric VERDICT classes for
  fleet-wide LIVENESS conflates two axes — answering/not-answering is not the same claim as
  "verified"/"declared but wrong". Both problems have one fix.
  DECIDED: drop the enc-text-* classes from the strip and give .strip .n its own colour
  rules from var(--good)/var(--crit), at a specificity that actually wins, with a comment
  saying these are liveness and deliberately not the verdict encoding. Add a test asserting
  the computed colour is not the default ink, so a future specificity change cannot silently
  kill it again. COST IF WRONG: two colour values stated outside the encoding table — which
  is correct here, because this is not a verdict.
Task 5: fix round 1/5 dispatched (Ruling 14)
Task 5: fix round 1/5 (1 addressed, 0 open — classes removed, data-figure rules at (0,3,0)
  beat .strip .n at (0,2,0), regression test added; commits e64bc4a..db8bc35). Measured after
  fix: answering rgb(12,163,12), not-answering rgb(208,59,59). Re-review: no new breakage.
Task 5: implementer checked before adding a computed-style test — tests/mobile/*.py use
  playwright but define no test_* functions and are not pytest-collected, so such a test
  would have been the suite's first playwright dependency. Took markup-level assertions
  instead and verified computed styles manually. Correct trade, and it said so.
Task 5: complete (commits 835a80e..db8bc35, review clean, 1 fix round)

Prepared context for Task 6 (verified): the index already has a bare `<input id="q"
  type="search">` at index.html:378 with a client-side filter script (index.html:~690-890)
  that reads data-endpoint and also drives facet chips. Task 6 must wrap it in a GET form
  with name="q", pre-fill it, and leave the client-side enhancement working over the rows
  the server returns.
Task 6: dispatched (base db8bc35, sonnet) — server-side ?q= on the HTML representation
Task 6: implemented (commit 3033bc7), test_index 78 passed/1 skip, suite 462 passed/1 skip.
  Implementer verified: / => 9 rows, /?q=uniprot => 1 row + "1 of 9", input round-trips
  value="uniprot", noscript button present, and Turtle output byte-identical with and
  without ?q= (confirming the RDF branch is genuinely untouched, as Task 7 requires).
Task 6: accepted rename — the brief's `rows_of` helper would have shadowed an existing
  unrelated `rows_of(_ignored, count, /, **cells)` in test_index.py. Renamed
  `endpoints_shown`; semantics identical. My ruling named a colliding helper; theirs is right.

Ruling 15 (Task 6, IMPORTANT, controller-found by measurement): the implementer flagged that
  `endpoint_count` now follows the filter. Measured against a real store what that does:
     /                  rows=9  description-sentence present
     /?q=uniprot        rows=1  description-sentence present
     /?q=zzzznomatch    rows=0  description-sentence GONE
  `endpoint_count` is never rendered as a number — it is only `{% if endpoint_count %}`
  guarding the site's own one-sentence description (index.html:334) and a facet message
  (:549). Filtered, it makes a no-match search hide what the site IS, leaving a near-empty
  page with no explanation of why.
  DECIDED, two parts: (a) `endpoint_count` reverts to the UNFILTERED count — it guards
  "does this store hold anything at all", a fact about the store, not about the query;
  (b) a no-match ?q= must SAY so, with a message naming the query and distinct from the
  empty-store case. My plan specified the denominator and never specified the empty-result
  state; that is a gap in the plan, not in the implementation.
  COST IF WRONG: an extra sentence on a page with no rows.
Task 6: fix round 1/5 dispatched (Ruling 15)
Task 6: fix round 1/5 (1 addressed, 0 open — endpoint_count back to unfiltered with a comment
  saying why it breaks the pattern; no-match message added with data-no-match, names the
  query, links back; 4 new tests; commits 3033bc7..6c53a7d). Re-measured:
     /                 9 rows, description present, figure "9",      no-match absent
     /?q=uniprot       1 row,  description present, figure "1 of 9", no-match absent
     /?q=zzzznomatch   0 rows, description PRESENT, figure "0 of 9", no-match present
  summary.total was computed before filtering and was never subject to the bug — verified.
Task 6: task review dispatched over the FULL range db8bc35..6c53a7d (sonnet) — as with
  Task 3, the task review had not run before the fix, so it covers both commits.
Task 6: review agent STALLED (watchdog, no progress 600s) mid-review; re-dispatched fresh. Partial output not trusted as a verdict.
Task 6: review returned spec ✅ / quality NEEDS WORK — one Important finding (client-side
  script drift), which is exactly what my check #4 asked the reviewer to look for.
  The reviewer traced it from source and marked it ⚠️ for want of a browser; I CONFIRMED IT
  LIVE with playwright:
     /                 shown='' ; #nothing hidden ; no-match absent      (correct)
     /?q=uniprot       shown='showing 1 of 1 endpoints' ; #nothing hidden
     /?q=zzzznomatch   shown='showing 0 of 0 endpoints' ; #nothing VISIBLE ; no-match VISIBLE
  So on a no-match query the reader sees TWO contradictory "nothing found" notices at once,
  and on any filtered page a "N of N" line that contradicts the strip's "N of 9".

Ruling 17 (Task 6, IMPORTANT): the inline script was written under the invariant "every row
  is already in the document" (its own comment at index.html:681). Task 6 broke that
  invariant and did not update the script. Its `total` (index.html:711) now counts the
  server-filtered rows, and because the input is pre-filled, apply() runs on load, sets
  filtering=true, writes a trivially-equal "N of N", and unhides the facet-empty paragraph.
  DECIDED: the client "shown" line and the #nothing paragraph describe CLIENT-SIDE narrowing
  only — facets, and typing beyond what the server already did. On a page that loaded with a
  server-applied q and no further interaction there is no client-side narrowing, so both stay
  suppressed. The script reads the unfiltered fleet size from a server-provided data
  attribute rather than counting rendered rows, so "of N" means the fleet again.
  COST IF WRONG: the shown-line's denominator is the fleet rather than the filtered set,
  which is the reading the strip already uses.
Task 6: fix round 2/5 dispatched (Ruling 17)
Task 6: fix round 2 implementer STALLED (watchdog, 600s) with a clean tree and no work done.
  That agent had been resumed twice and carried ~250k tokens. Second stall of the session,
  both on long-lived agents. DECIDED: dispatch a FRESH implementer for round 2 rather than
  resume a third time — this is a harness/context failure, not a quality escalation, so the
  model tier stays the same. The report file is the persistent memory either way.
Task 6: fix round 2/5 (1 addressed, 0 open — fleet total from data attribute, `interacted`
  flag on BOTH the input and the facet-chip listeners, serverNoMatch makes the two no-match
  notices structurally exclusive, stale invariant comment corrected; commit 7ecaa00).
  Re-review: all three parts addressed, no new breakage, no interaction path missed.
Task 6: complete (commits db8bc35..7ecaa00, review clean, 2 fix rounds)
Task 7: brief RE-EXTRACTED after the Ruling 16 plan amendment (1ee5469) so it carries the
  corrected object-side filter rather than the subject-substring one.
Task 7: dispatched (base 7ecaa00, sonnet) — ?q= on the RDF representation
Task 7: review returned spec ❌ / quality NEEDS WORK — one Critical, one Important.
  I VERIFIED THE CRITICAL MYSELF and the reviewer is right on both counts.

Ruling 18 (Task 7, CRITICAL, MY MEASURED FACT WAS WRONG): Ruling 16 asserted "an endpoint is
  never a SUBJECT in this document", measured against run-registry-sample.nq. That fixture
  has no dormancy. Re-measured with run-with-samples.nq + run-with-dormancy.nq:
     ENDPOINT IRIs AS SUBJECT: 1  (https://data.kkg.kadaster.nl/query)
     after _only_matching_endpoints(ts, 'ontop'): 2 non-matching endpoint IRIs still present
       - https://data.kkg.kadaster.nl/query   (dormancy facts, endpoint as SUBJECT)
       - https://qlever.dev/api/osm-planet    (sw:completedEndpoint, as OBJECT on the activity)
  I measured ONE fixture and generalised, then restated it to the implementer AND to the
  reviewer as a measured fact, including the false claim that activity nodes carry no
  per-endpoint link — they carry sw:completedEndpoint and sw:dormantEndpoint. The
  implementation followed my brief faithfully; the defect is mine.
  DECIDED: the filter is closed over the ENDPOINT SET, not over an enumerated predicate list,
  so a predicate added to the CONSTRUCT later cannot silently reopen the leak. Rule A drops
  any triple mentioning a known non-matching endpoint in subject or object; Rule B keeps the
  existing computedOn/notMeasuredOn subject pass for nodes that describe an endpoint without
  naming it. Rule B's subject set stays derived from those two predicates ONLY — deriving it
  from any object match would drop the activity node and the sweep's provenance with it.
  The test asserts the closed invariant, not a predicate list.
  COST IF WRONG: an over-broad filter could drop a triple that merely mentions an endpoint
  in passing; bounded by "known endpoint" coming from `entries`, so no vocabulary IRI is
  ever mistaken for one.

Ruling 19 (Task 7, Important): `list(store.query(...))` materialises the whole constructed
  graph on EVERY request, including unfiltered ones, where it used to stream into serialize().
  Gratuitous — list() is only needed inside the `if q:` branch. DECIDED: materialise only
  when filtering. COST IF WRONG: none.
Task 7: fix round 1/5 dispatched (Rulings 18 and 19)
Task 7: fix round 1/5 (2 addressed, 0 open — commit 2cb6b40). Implementer counts: 0 leaked,
  5 provenance triples surviving. Controller independently re-measured: 0 leaked, 7
  activity-subject triples surviving (different counting basis, consistent), and app.py:2533
  now reads `store.query(...)` with no list() on the unfiltered path (Ruling 19 addressed).
  Implementer also confirmed the NEW TEST IS DISCRIMINATIVE by checking the old
  predicate-list filter fails it — a test that could not fail would have been worthless here.
Task 7: re-review — both findings ADDRESSED, no new breakage. known_endpoints comes from
  endpoint_index(store), the canonical list, NOT from the constructed triples, so a
  dormancy-only endpoint cannot be missed. Rule B stays gated on the two predicates, so the
  provenance trap is avoided by construction and the docstring now says so. Reviewer reverted
  app.py to pre-fix, confirmed the new test FAILS (discriminative), and restored.
Task 7: complete (commits 7ecaa00..2cb6b40, review clean, 1 fix round)
Task 8: dispatched (base 2cb6b40, sonnet) — endpoint.html column+rail; also closes the
  Ruling 12 temporary regression (that page's verdict chips have been unstyled since Task 5).

Prepared context for Task 11 — every claim VERIFIED by the controller rather than carried
over from the spec review that first raised them (one unverified claim has already cost a
Critical this session):
  - probe_mobile.py:15 VIEWPORTS = 375 / 412 / 430. The plan's original "400px" was wrong;
    the amended text is right.
  - probe_mobile.py:20-22 lists SEVEN routes: /, /about, /docs, /docs/metrics, /docs/states,
    /explore, /endpoint?url=... — /docs/void is genuinely absent. Task 11 adds it.
  - templates/docs-void.html:2 is `{% block title %}States{% endblock %}` — the pre-existing
    wrong title survived Task 3's conversion verbatim, exactly as the no-copy-changes rule
    required. Ruling 10 fixes it in Task 11.
Task 8: implemented (commit 882c036), test_page 90/90, suite 475 passed/1 skip.
Task 8: controller verification — endpoint.html no longer declares its own token block (0
  matches for `bg: #0a1929`). Rendered at 1200/390px: overflow 0px at both; rail
  position:sticky at 1200px and static at 390px; .enc-verified chips compute
  background rgba(0, 0, 0, 0.2) — the measured fill, actually applied.
  RULING 12'S TEMPORARY REGRESSION IS CLOSED: that page's chips have been unstyled since
  Task 5 and now draw from the shared --fill.
Task 8: accepted deviation — two new tests moved from the `store` fixture to
  `store_content_profiles`. The implementer MEASURED that `store` yields
  void_summary() == None, so that endpoint page has no vocabulary section and can carry at
  most 2 rail facts, making the brief's assertions unsatisfiable against it regardless of
  implementation. test_the_sample_survives_an_endpoint_with_no_vocabulary still uses
  store_sampled_profile — verified, and that is the one guarding the no-folding ruling.
Task 8: minor (deferred): the rail's "last checked" renders a raw ISO timestamp
  (2026-09-05T14:00:00Z) and wraps mid-token in the 258px rail. Ruling 7 sliced the index
  strip's equivalent to [:10] for exactly this reason but said nothing about the rail, so
  the two now disagree. Cosmetic; final review to triage.
Task 8: review dispatched (sonnet)
Task 8: review returned spec ✅ / quality NEEDS WORK — two Important. BOTH VERIFIED by me.

Ruling 20 (Task 8, IMPORTANT, MY BRIEF'S TEST CANNOT FAIL): the test guarding the
  no-folding ruling — test_the_sample_survives_an_endpoint_with_no_vocabulary — uses
  store_sampled_profile, which I specified. Rendered that fixture:
     data-section = conformance, sample, vocabulary, void
  It HAS vocabulary. So a test named "survives an endpoint with no vocabulary" runs against
  an endpoint that has one, and would keep passing if a future implementer folded the sample
  into {% if vocabulary %} — the exact regression it exists to prevent. Same failure class as
  Task 7's non-discriminative test: a test that certifies the bug it is meant to catch.
  Verified the alternative the reviewer proposed: `store` (run-with-samples.nq) + the
  kadaster endpoint renders data-section = conformance, sample — sample present, no
  vocabulary, no void. That is the shape required.
  DECIDED: retarget the test at that fixture and endpoint, AND have it assert its own
  premise — that the vocabulary section is ABSENT — so the fixture cannot drift later and
  silently disarm the guard again. COST IF WRONG: none; strictly more discriminative.

Ruling 21 (Task 8, IMPORTANT): endpoint.html:172 `.fact:last-of-type { border-bottom: none }`
  is inert. :last-of-type matches by TAG among siblings, not by class, and the rail's last
  <div> child is always div.legend (endpoint.html:511), so no .fact is ever :last-of-type.
  Cosmetic (one extra hairline) but the rule never fires in any rendering path.
  DECIDED: fix it to target the actual last fact. COST IF WRONG: a hairline.

Task 8: minor (deferred): an unrequested `&mdash;` -> literal em dash change in the
  history-crosshair script, a block the brief did not ask to touch. Behaviour-neutral.
Task 8: fix round 1/5 dispatched (Rulings 20 and 21)
Task 8: fix round 1/5 (2 addressed, 0 open — commit 7e36c03). Guard retargeted at
  `store` + KADASTER, asserts its own premise (no vocabulary section present), and its
  docstring records why store_sampled_profile was wrong so nobody swaps it back.
  Implementer PROVED discrimination: wrapped the sample section in {% if vocabulary %},
  the test FAILED, reverted, 90/90 again. Dead .fact rule fixed and confirmed by render
  (first 3 rows keep border-bottom, last shows none).
Task 8: re-review dispatched (haiku)
Task 8: re-review — both findings ADDRESSED, no new breakage. CSS fixed by wrapping the
  facts in a .facts container so `.facts .fact:last-child` matches, rather than patching the
  broken :last-of-type in place. KADASTER pre-existing at test_page.py:63.
Task 8: complete (commits 2cb6b40..7e36c03, review clean, 1 fix round)
Task 9: dispatched (base 7e36c03, sonnet) — vocab_match.py, carrying Rulings 1 and 2
Task 9: implemented (commit 73a32cf), test_vocab_match 19/19, suite 494 passed/1 skip.
  Three new files only; no existing file touched.
Task 9: controller verification — score_word('recpetor',['receptor'],...) = 1 (Damerau
  transposition confirmed), deletion = 1, insertion = 1, 3-letter 'dgu' = 0 (floor holds).
  All four shared fixture cases pass, incl. 'drug target' ->
  ['DrugTarget','targetOfDrug','Target','Pathway'] in order, bands [match,match,match,close].

Ruling 22 (Task 9, MY BRIEF CONTRADICTED ITSELF): the brief's
  test_two_words_in_the_wrong_order_still_match used a distractor term "Unrelated" with
  prefix "drugbank". That prefix collides with the query word "drug" through the very
  namespace-match rule the ADJACENT test (test_a_namespace_match_lands_in_the_close_band,
  via Pathway) exists to preserve — and which Ruling 2 deliberately kept. Run verbatim, my
  own reference implementation fails my own test: Unrelated lands in the close band instead
  of being excluded. The two tests as written contradict each other.
  The implementer changed that distractor's prefix to "other", in test_vocab_match.py only,
  with a comment. DECIDED: correct and minimal — it preserves both tests' intents and leaves
  tokenize/score_word/rank semantics and the shared vocab_match_cases.json untouched, so
  Task 10's JS contract is unaffected. COST IF WRONG: none.
Task 9: review dispatched (sonnet)
Task 9: review returned spec ✅ / quality approved, with one Important and two Minors.
  Reviewer BRUTE-FORCED _within_one_edit against a reference restricted-Damerau DP over all
  string pairs of length 0-5 on a 3-letter alphabet (132,496 pairs): zero mismatches. It
  correctly rejects two separate transpositions and two substitutions masquerading as one.
  It also confirmed the test table is DISCRIMINATIVE: the recpetor case fails under plain
  Levenshtein, the Pathway case fails under a close-band requiring all words.

Ruling 23 (Task 9, IMPORTANT): the sort's tie-breaks are not TOTAL. Verified myself — two
  terms sharing `local` but differing in prefix compare equal on every key, so Python's
  stable sort falls back to input order:
     input [rdfs, skos] -> ['rdfs', 'skos']
     input [skos, rdfs] -> ['skos', 'rdfs']
  The module's own comment claims the order "cannot shuffle between keystrokes", which holds
  only if the caller's input order is itself stable. Same-named terms across vocabularies
  (label, id, type, name) are ordinary in RDF. DECIDED: add a final tiebreak on `iri`, which
  is unique per term, making the order total. This must land BEFORE Task 10, which
  transliterates this exact ordering into JS and would otherwise carry the gap across.
  COST IF WRONG: none; a total order is strictly more defined than a partial one.

Ruling 24 (Task 9, contract coverage): vocab_match_cases.json has no 3+-word query, so the
  "at least half, rounded up" band arithmetic is untested by the shared contract — and that
  file is exactly what Task 10's JS is checked against. A JS port that rounded DOWN would
  pass the suite as it stands. Verified the arithmetic is right (2-of-3 qualifies, 1-of-3
  does not); the gap is in coverage, not behaviour. DECIDED: add a 3-word case to the shared
  fixture before Task 10 locks the contract. COST IF WRONG: one more fixture case.

Task 9: minor (deferred): tokenize() does not split acronym-to-word boundaries —
  HTTPRequest -> ['httprequest'], IRIValue -> ['irivalue'], while hasIRI -> ['has','iri']
  works. Inherited from my regex, not an implementer defect. NOT changing it now (a
  tokenisation change is a behaviour change beyond this brief), but Task 10 must reproduce
  the limitation rather than silently "fix" it and diverge from Python.
Task 9: fix round 1/5 dispatched (Rulings 23 and 24)
Task 9: fix round 1/5 (2 addressed, 0 open — commit c45d347). Ordering now total via an
  `iri` tiebreak: verified rank([a,b]) == rank([b,a]) for two terms sharing `local`.
  Three-word case 'alpha beta gamma' added to the shared contract.
Task 9: MY REQUESTED EXPERIMENT WAS WRONG, and the implementer said so rather than fudging
  it. I asked them to prove the new case by flipping `count*2 >= len(words)` to `>`. They
  ran it, found the new case did NOT fail (two pre-existing two-word cases did), and
  explained why: count*2 is always even, so on an ODD word count >= and > admit exactly the
  same counts. Verified myself:
     2-word: >= admits [1,2],   > admits [2]      -> differ
     3-word: >= admits [2,3],   > admits [2,3]    -> IDENTICAL
     4-word: >= admits [2,3,4], > admits [3,4]    -> differ
     5-word: >= admits [3,4,5], > admits [3,4,5]  -> IDENTICAL
  My mutation could never have exercised a three-word case. They then found the mutation
  that does — `count >= len(words) // 2`, floor rounding, which wrongly admits the 1-of-3
  term — and confirmed the new case fails under it. Reverted both mutants; diff is clean.
  This is a better piece of reasoning than the instruction it replaced.
Task 9: re-review dispatched (haiku)
Task 9: re-review — both ADDRESSED, no new breakage. iri tiebreak is last in the sort tuple
  with a safe "" default; pre-existing fixture orders unchanged; tokenize/_BOUNDARY untouched
  with the acronym limitation now documented for the Task 10 port; no leftover mutations.
Task 9: complete (commits 7e36c03..c45d347, review clean, 1 fix round)
Task 10: dispatched (base c45d347, sonnet) — JS transliteration + tokens field + script route
Task 10: implemented (commit dbbbc05), suite 496 passed/1 skip. The node agreement test RAN
  and PASSED (node present), it did not skip.
Task 10: controller verification — ran the JS directly under node on the four contract
  properties, including the two the 5-case fixture does NOT cover:
     transposition/deletion/insertion -> 1, 1, 1   (Damerau reproduced)
     three-letter floor               -> 0
     tokenize HTTPRequest -> ['httprequest'], IRIValue -> ['irivalue'],
       hasIRI -> ['has','iri']                     (acronym limitation preserved, not "fixed")
     rank([a,b]) == rank([b,a]) == ['x:a','x:b']   (total order holds in JS too)
     'alpha beta gamma' -> ['AlphaBeta'] only      (ceiling arithmetic, 1-of-3 excluded)
  Every one matches Python.
Task 10: implementer found a LATENT CSS bug outside its file list, by driving the page:
  `.vocab li { display: grid }` outranked the UA's `[hidden] { display: none }`, so hidden
  rows never disappeared. Predates Task 10. Fixed at endpoint.html:152.
Task 10: OPEN QUESTION for review — the agreement test needs `--experimental-detect-module`
  on node 20 because a bare .js with no package.json defaults to CommonJS. Where node is
  ABSENT the test skips cleanly; where node is PRESENT BUT OLDER than that flag, it would
  ERROR rather than skip. Worth assessing.
Task 10: review dispatched (sonnet)
Task 10: review returned spec ✅ / quality approved, with one Important and two Minors.
  Reviewer independently hit the script route through a TestClient (200,
  content-type application/javascript, immutable header, served bytes hash to the digest in
  the path, bogus digest 404s), confirmed vocab_match.py is byte-for-byte untouched, and
  found no defect in the DOM half (row data read once at load, not per keystroke; clearing
  restores exact server order via one fragment).

Ruling 25 (Task 10, IMPORTANT): the agreement test guards only on
  `shutil.which("node") is None`, but needs `--experimental-detect-module`. On a machine
  where node is PRESENT BUT OLDER than that flag, node exits "bad option" and
  subprocess check=True raises, so pytest reports an ERROR, not a skip — breaking the
  "machines without the toolchain just skip" contract the guard exists to honour.
  Verified the alternative myself: copying the script to a .mjs path and importing it works
  with NO flags at all (`node -e "import('/tmp/vs.mjs')..."` -> rank is function). .mjs has
  been unambiguous ESM since node 12.17.
  DECIDED: switch to the .mjs copy, drop the flag. COST IF WRONG: a temp file per test run.

Task 10: minor (folded into this fix): the CSS comment at endpoint.html:148-152 attributes
  the [hidden] override to SPECIFICITY. The real cause is cascade ORIGIN — author rules beat
  user-agent rules regardless of specificity. The fix works for a different reason than
  stated, and comments here explain *why*, so a wrong why misleads.
Task 10: minor (DEFERRED TO TASK 11): no regression test for the new script route, where the
  stylesheet route has three in test_static.py. Asymmetry worth closing; Task 11 is the
  natural home. Reviewer verified the route by hand meanwhile.
Task 10: minor (noted, no action): data-hay is now inert for the JS, which recomputes the
  haystack from local/prefix/iri. Still covered by its own pre-existing test; kept per brief.
Task 10: fix round 1/5 dispatched (Ruling 25 + the comment)
Task 10: fix round 1/5 (2 addressed, 0 open — commit 74d731d). Flag removed entirely; .mjs
  copy via tmp_path (unique, auto-cleaned, byte-identical); guard retained; same fixture,
  full comparison. CSS comment now states cascade origin correctly.
Task 10: complete (commits c45d347..74d731d, review clean, 1 fix round)
Task 11: dispatched (base 74d731d, sonnet) — closing pass, carrying three items the
  originally-extracted brief does not have: Ruling 10 (docs-void title), the Task 10
  deferred minor (script-route regression tests), and the corrected probe facts.
Task 11: implemented (commits 3830575, 22af325). Suite 499 passed/1 skip; prober cargo test
  green and clippy -D warnings clean (this plan never touched the prober; run to confirm).
Task 11: controller verification of the headline claims — templates 155,598 bytes (from
  190,395); style lines ~750 (from 1,427); `grep -- '--accent:' web/templates/` CLEAN;
  docs-void title now "VoID"; 3 script-route tests present; /docs/void in the probe.
  Probe: 8 routes x 3 viewports, 0px overflow throughout, nothing needed restoring.
  Cache proven via CDP, not eyeballed: / fetched the hashed CSS at transferSize 10252,
  the next page requested the same URL at transferSize 0 (browser cache), identical
  encodedBodySize both times.
Task 11: fix round 1/5 (README route count 7->8; probe docstring corrected comment-only
  after the implementer judged it would not change behaviour — the right call, and it said
  which it was doing). Probe re-run after: 8 routes x 3 widths, 0px.
Task 11: task review dispatched (sonnet)
Task 11: review clean — spec ✅, quality approved, no Critical/Important. Reviewer
  MUTATION-TESTED each of the three new script-route tests against the real route: removing
  the digest check fails only the wrong-hash test; serving stale bytes with correct headers
  fails only the content test; weakening Cache-Control fails only the immutable test. Each
  fails exactly for the property it names — a faithful mirror, not a weaker one. It also
  recomputed the fill contrast from the actual site.css hex values, alpha-compositing and
  rounding to integer pixels as a browser would, and got 1.598:1 and 1.627:1 exactly,
  independently deriving the same composited #cacac9 the spec records. No drift between
  verdict-encoding.md, verdict_encoding.py and site.css.
Task 11: minor (deferred): the PRE-EXISTING docstring of
  test_the_verdict_fill_is_visible_on_both_surfaces says the working fill "measures 1.623:1"
  as one number. That was my original dark-surface measurement on #0a1929; the per-surface
  values are now 1.598 (light) and 1.627 (dark). Loose, not wrong-in-kind, and out of
  Task 11's diff. Final review to triage.
Task 11: complete (commits 74d731d..22af325, review clean, 1 fix round)

=== ALL 11 TASKS COMPLETE ===

=== FINAL WHOLE-BRANCH REVIEW (opus) — 2 must-fix, both VERIFIED by controller ===

Ruling 26 (FINAL, MUST-FIX, and it is the defect this whole redesign exists to prevent):
  explore.html:128-136 carries a stale private copy of the verdict encoding with
  `background: rgba(255,255,255,0.16)` hard-coded. head_extra loads AFTER the hashed
  stylesheet, so it overrides the generated var(--fill). Measured live on /explore, light:
     .enc-verified background = rgba(255, 255, 255, 0.16)
     16% white over #fcfcfb composites to (252,252,252) -> contrast 1.004:1
     floor 1.4 ; known-invisible 1.139 ; correct fill 1.598
  1.004:1 is SIX TIMES further below the floor than the value this codebase already proved
  invisible. Injectivity fails on that page: verified/declared-only/absent all become
  (solid,1) and undeclared-but-verified/indeterminate both (dashed,1) — four states into
  two, exactly as verdict-encoding.md (added by this branch) warns. And explore's term
  marks carry only a title, no visible label, so a mark appears there with no working third
  channel AND no text.
  Introduced BY this branch: pre-branch the site was dark and 16% white was correct.
  Worse: test_explore.py:123 asserts `f"enc-{state}" in body`, and those literals appear
  ONLY in the stale block — so a test currently PINS THE DEFECT IN PLACE.
  Root cause of the miss: every page got a "no inline tokens" test; only 2 of 8 got a
  "no private encoding copy" test. DECIDED: delete the block, re-point the test at the
  served stylesheet, and generalise the no-private-copy test to ALL pages.

Ruling 27 (FINAL, MUST-FIX): app.py:2254 `history = fleet_history(store)` reads the store
  directly and never sees `q`, while entries are filtered at :2248. Verified live:
     GET /?q=kadaster  rows: data-endpoint = kadaster
                       grid: data-fleet-endpoint = ontop   <- filtered out, still LINKED
  So the HTML names and links an endpoint the RDF representation of the same URL drops.
  This is the one path that bypasses _matches_query, and the negotiation tests cannot see it
  because both read data-endpoint, which never matches data-fleet-endpoint. The committed
  fixtures happen to have <=1 changed endpoint, which is why nothing went red.
  Same section also states "All 4 read the same way" beside a strip reading "1 of 4".
  DECIDED: filter history.rows through _matches_query (leaving history.runs, which is
  service-level), give the sentence its denominator, and add a test asserting
  data-fleet-endpoint is a subset of data-endpoint under a filter.

Ruling 28 (FINAL): the JS agreement test cannot see a band error — the reviewer mutated
  vocab-search.js to label every term "match" (presenting a typo hit as exact) and the suite
  passed 21/21, because the fixture's expected holds only `local` names. Python is covered;
  the transliteration is not. DECIDED: add `band` to the shared fixture's expectations and
  compare it on both sides. Same category as every other can't-fail test this session.

Ruling 16 (BOOKKEEPING REPAIR — recorded here late). This ruling was written into the plan
  amendment at commit 1ee5469 and its content is quoted inside Ruling 18, but it was never
  logged in this ledger under its own number, so a reader counting rulings would find a gap.
  Its content: "filter the RDF index on the LINK (dqv:computedOn / sw:notMeasuredOn) rather
  than by substring-matching each subject's text, because measurement URNs carry the
  endpoint percent-encoded and a subject-substring filter agrees with the HTML only by
  accident." It was correct as far as it went and was SUPERSEDED by Ruling 18, which found
  its premise ("an endpoint is never a subject") held only for the fixture I measured.
  Recorded so the ruling list handed to the owner is complete rather than tidy.

=== SPEC REQUIREMENTS APPROVED BUT NEVER BUILT (found by the final review, verified by me) ===
  1. Spec §5 (line 373): "Matched spans are wrapped in <mark> for scores 4, 3 and 2 only."
     Verified: no <mark> anywhere under web/. Never appeared in the plan, so no task built it
     and no task review had reason to look for it.
  2. Spec §2 (line 214) and §6 (line 401): the docs pages gain a sub-nav from _docs_context().
     Verified by rendering: /docs links to all three children, but /docs/metrics,
     /docs/states and /docs/void link to NO siblings — they are dead ends reachable only
     back through the header. The hub works; the leaves do not.
  Neither is a defect in any task's execution. Both are gaps between the spec the owner
  approved and the plan I wrote from it. FOR THE OWNER TO DECIDE, not for me to slip in.
FINAL FIX WAVE: commit 929d853, suite 501 passed/1 skip. All three findings fixed, each with
  discrimination proven:
   - /explore .enc-verified now computes rgba(0,0,0,0.2) = 1.604:1 (was 1.004:1). CONTROLLER
     VERIFIED LIVE.
   - data-fleet-endpoint under ?q=kadaster now empty (was ontop, outside the filter).
     CONTROLLER VERIFIED LIVE. Strip and sentence agree: "1 of 3" / "All 1 of 3".
   - JS band mutation now CAUGHT: with band forced to "match", the agreement test fails on
     ['match'] != ['close'] while the old name-only assertion still passed — reproducing the
     blind spot exactly. Reverted, 21/21.
   - The new all-routes test FAILED before the explore deletion, naming /explore
     specifically, and passes for all eight after.
FINAL FIX WAVE: two honest concerns from the implementer, both recorded not waved:
   - the denominator uses summary.total; equal to the matching count in every case it could
     construct, but not proven for an entry with no fleet-history row.
   - NO committed fixture has two endpoints that ever changed between sweeps (it checked all
     of them), so the new subset test pins the property by excluding then including the one
     changed endpoint rather than by a genuine two-endpoint case. Noted in the test docstring.
FINAL FIX WAVE: scoped re-review dispatched (sonnet). Per the skill there is no second fix
  wave — any residual findings surface to the owner.
FINAL FIX WAVE: re-review — all 3 findings + the added all-routes test ADDRESSED, no new
  breakage, VERDICT: FIT TO MERGE. Reviewer confirmed FleetHistory has exactly two fields
  (runs service-level, rows per-endpoint) so nothing per-endpoint was missed, and that the
  re-pointed explore test is STRONGER than the original (the old one was satisfiable by JS
  source text alone and never checked the CSS existed).
RESIDUAL, NON-BLOCKING, surfaced to the owner rather than fixed (no second fix wave per the
  skill): the fleet sentence's denominator assumes every matching entry has a fleet-history
  row. `entries` comes from the whole registry; fleet_history.rq emits a row only for an
  endpoint some run actually measured. A newly-registered, never-swept endpoint would make
  fleet.steady undercount. It omits rather than misstates. No fixture exercises it.
=== BRANCH COMPLETE: 24 commits, 501 passed / 1 pre-existing skip ===
