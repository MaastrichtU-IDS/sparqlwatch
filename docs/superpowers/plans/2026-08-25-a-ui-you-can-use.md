# Stage 3-2: A UI You Can Use

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the 543-endpoint registry browsable, and make all three read paths survive a year of daily sweeps.

**Architecture:** A derived `urn:sparqlwatch:current` graph, maintained by `web/load_run.py` with one atomic `store.update()` per endpoint, holding each endpoint's facts from its newest run and a pointer to which run that was. All three read queries move to it in one commit. Then the index, and `/about`.

**Tech Stack:** Python 3.12, pyoxigraph 0.5.9, FastAPI, Jinja2. No new dependencies. No JavaScript framework: the filter is a few lines of vanilla JS over rows already in the document.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## This plan was rewritten after review

The first draft was reviewed before execution and found to contain **10 Critical, 13 Important and 6 Minor defects**. The review is at `.superpowers/reviews/2026-08-25-plan-3-2.md` and re-derived every number. Read it. Nine findings changed the design rather than the wording, and an implementer who does not know them will reintroduce them:

1. **A `current` graph carrying an activity is a second run to every existing query.** All three read queries select their run as `GRAPH ?run { ?activity a prov:Activity ; prov:generatedAtTime ?generatedAt }`, with no restriction on which graph, and the newest-run aggregate is unrestricted too. So a `current` graph holding a typed activity with a timestamp **is** a run, and it carries the newest timestamp by construction. Measured on a committed fixture: `endpoint_measurements` and `endpoint_content` both raise `2 runs tied as most recent`, so every page and every RDF representation becomes a 500.
2. **The suite cannot see that.** Every page, negotiation, measurement and content test builds its store with `store.load(...)`, never through `load_run()`, so no test store will ever contain a `current` graph. Task 1 could commit green and break every page.
3. **`Store.update()` is transactional**, and the claim at `load_run.py:30-38` that pyoxigraph 0.5.9 offers no transaction is false and has been shaping this project's design since stage 2-1. The docstring says "either the full operation succeeds, or nothing is written", and the reviewer verified it holds across `;`-separated operations. So per-endpoint atomicity is available and the first draft accepted a hazard the library does not impose.
4. **All three read paths compute recency identically, and the first draft's excuse for leaving two of them alone was a vacuous measurement.** `endpoint_content.rq` measured 0 ms because the registry sweep declined `classes` for all 543 endpoints, so the query matched nothing at any depth. With one sample per endpoint per run it is **6,488 ms** at 30 runs, and `endpoint_description.rq`, which is the entire RDF representation, is **11,704 ms**, twice the page this stage exists to fix.
5. **Moving only the HTML breaks the property stage 3-1 was built to guarantee.** A resource with two representations may not have two notions of "newest".
6. **The update rule was monotone-only.** A same-run reload ties rather than advances, so the first draft's stated recovery ("re-load the run") is precisely the operation that does nothing; a run graph that shrinks leaves `current` attributing facts to a run that no longer states them; and the finished/unfinished flip keeps saying "did not finish" about a sweep that did.
7. **A store built before this stage answers "we know nothing about this endpoint"** for endpoints it fully describes. That is the state of the only real store this project has.
8. **The index's headline claim was wrong for four endpoints.** Availability across the 543 is 57 `verified`, 482 `indeterminate` and **4 `absent`**, and `absent` is common on other metrics of non-responders (`cors` 19, `cors-preflight` 78, `geo-data` 52). There is no such thing as "the 486 that show indeterminate".
9. **"Whether the endpoint answered a query" is not the availability verdict.** `absent` means the host answered with something that was not a SPARQL result, `indeterminate` covers a timeout, a transport error, a DNS failure or an HTML front end. A heading of "did not answer" over the four `absent` rows says something false, and putting 482 `indeterminate` rows under it turns "we could not determine this" into a determined negative, which is the one thing the conformance model exists to prevent.

## The measurement, corrected

Re-derived by the reviewer from `~/code/sparqlwatch-runs/run-2026-08-24T19-45-03Z-lod-cloud-543.nq`, replayed under 1, 7 and 30 run IRIs with the run instant rewritten everywhere it appears:

| history | quads | `endpoint_measurements.rq` | `endpoint_content.rq` | `endpoint_description.rq` |
| --- | --- | --- | --- | --- |
| 1 run | 27,194 | 10.0 ms | 0.10 ms | 2.0 ms |
| 7 runs | 190,358 | 309.7 ms | 0.15 ms | 13.4 ms |
| 30 runs | 815,820 | **5,801.8 ms** | 0.17 ms | 78.9 ms |

The load-bearing column holds to within 3% of my own figures, so **the 5.8-second page is real**.

**The content and description figures are measured over a store with zero content samples.** With one sample per endpoint per run, which is what stage 2b exists to produce:

| history | `endpoint_content.rq` | `endpoint_description.rq` |
| --- | --- | --- |
| 1 run | 1.2 ms | 3.3 ms |
| 7 runs | 115.6 ms | 200.0 ms |
| 30 runs | **6,488.5 ms** | **11,704.3 ms** |

(Caveat the reviewer stated and this plan keeps: that synthetic store samples every endpoint in every run, which is the steady state only if `classes` runs at an expensive ceiling every sweep. A rarer expensive sweep makes the growth slower and the shape identical.)

**The first draft's index column is withdrawn.** It gave no query, and nothing the reviewer wrote landed on 279 / 13,812 / 256,881 ms: three defensible shapes came out at 16 / 484 / 7,249 ms, 1,726 / 79,567 ms, and 3,379 / 148,456 ms. Two of them bracket the withdrawn figure, one 17x below and one 12x above, so it cannot be checked and must not be published. **The thesis is unaffected: the cheapest defensible index shape is still 7.2 seconds at 30 runs.** And do not repeat the first draft's "two orders of magnitude" or "543 times over": measured against the page, the index is about **44x**, not 543x.

**What `current` buys, measured:** a flat scan of a hand-built `current` graph over the 30-run store is **3.1 ms** for 3,801 rows. A per-endpoint rebuild of that graph as one SPARQL update over the same store is **43.5 s**, which is why rebuild is a repair tool and not the maintenance mechanism.

## What the spec says, and where the code disagrees with it

The spec at `:113-115` says the derived graph "holds the latest measurement per **(endpoint, metric)**" and is "**rebuilt** after each run, never hand-edited". The first draft paraphrased this as per-endpoint and incremental, and got both halves wrong.

**This plan keeps per-endpoint semantics and says why, which means the spec's wording is what changes.** `endpoint_measurements.rq:45` already rules that recency is per endpoint and not per
individual metric, on the ground that one sweep either measured an endpoint or it did not
(paraphrased; read the comment), and that reasoning is better than the spec's: mixing metrics from different runs produces a row whose parts came from different sweeps, which is exactly the Critical stage 3-1 was fixed for (a 59-class sample attributed to a sweep that declined `classes`). So Task 5 corrects the spec to per-endpoint and records that the code's reasoning won.

**Rebuild-after-each-run is also what changes.** At 43.5 s per run it is not a maintenance mechanism. Incremental per-endpoint updates at load time are, with rebuild kept as the repair path.

## What this slice is, and what it deliberately is not

The designed directory is `design/Main.dc.html`: a task-first surface with capability chips, a text filter, and facets over **vocabulary and class**. Those two facets have no data. `classes` is expensive, the registry sweep ran cheap, and it produced **zero** content samples; properties per class were never built. Building them now means filtering over empty sets and calling it a feature.

**In scope:** `current`, all three read paths moved onto it, the index, `/about`, and the exclusion mechanism `/about` needs in order not to lie.

**Not in scope, and each because the data or the stage is not there:** the vocabulary and class facets, the task-first surface, per-metric pages, history and charts, the embedded query editor, and the read-only public SPARQL endpoint. Say so on the page.

## Global Constraints

- Python 3.12 in `web/.venv`. **No new dependencies.** Both suites green at every commit.
- **Never report a confident wrong answer.** The six-verdict vocabulary is closed plus the non-verdict `NotMeasured` and `ContentSample` facts. **Render the verdict the store holds, never a substitute.** No row may display a verdict the store does not hold for that endpoint and metric.
- **Run graphs stay immutable and append-only.** `current` is derived, so it may be rewritten, but nothing in it may contradict a run graph, and it must be reconstructible from the run graphs alone.
- **`current` contains no `rdf:type prov:Activity` triple**, ever. The run an endpoint's facts came from is named by a pointer predicate on the endpoint, not by re-typing an activity in a second graph.
- **The verdict encoding table stays in one place**, `web/verdict_encoding.py`. The index's chips and group labels derive from it.
- **The spec requires content negotiation on every resource.** The index is a resource, so it negotiates too.
- **No em-dashes** anywhere. Every comment and doc sentence defensible by pointing at a line of code or a line of the measurement above.
- Commit in logical steps, staging by name. Never `git add -A`.

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `web/load_run.py` | modify | Maintains `current` with one atomic `store.update()` per endpoint. Gains rebuild and check modes. Its false transaction claim at `:30-38` is corrected. |
| `web/tests/conftest.py` | modify | Fixture stores go through `load_run()` so the suite can see `current` at all. |
| `web/app.py` | modify | `_opened_store` refuses a store holding run graphs and no `current`, naming the rebuild command. `GET /`, `GET /about`. |
| `web/queries/endpoint_measurements.rq`, `endpoint_content.rq`, `endpoint_description.rq` | modify | All three read `current`, in one commit. |
| `web/queries/index.rq`, `web/endpoint_index.py` | create | Every endpoint in `current` with its verdicts. A flat scan. |
| `web/templates/index.html`, `about.html` | create | Layout and theme tokens from `design/Main.dc.html`; verdict encoding from `verdict_encoding.py`. |
| `prober/registry/exclusions.toml`, `prober/src/registry.rs`, `prober/src/seed.rs` | create/modify | An excluded URL never reaches a sweep, and a re-seed preserves the exclusions. |

---

### Task 1: `current`, and all three read paths onto it

**Files:**
- Modify: `web/load_run.py`, `web/tests/conftest.py`, `web/app.py`, all three `web/queries/*.rq`
- Test: `web/tests/test_load_run.py` and the existing page, content and negotiation tests

**This is one task because the review showed the two halves are inseparable.** A `current` graph that no reader uses is dead weight; a reader that reads a graph no fixture has returns zero rows and renders "we know nothing about this endpoint" for every fixture endpoint. Splitting them means one commit is red by construction, and the tempting fix, "read `current`, and if it says nothing compute recency the old way", passes every test while keeping the 5.8-second path alive for exactly the stores that matter and creating two derivations that can disagree with nothing able to tell.

**The exact shape of `current`, and it holds pointers rather than copies of run-level facts.**
For each endpoint: its `MeasurementRow` quads, its `declarationsRead`, its `NotMeasured`
facts, and **two pointers**, `sw:currentRun` and `sw:currentSampleRun`. **No
`rdf:type prov:Activity`, no exceptions.** Write the quad shape out in the loader's
docstring; a reader should not have to infer it.

**Why the run-level facts are reached through the pointer and not copied.** An earlier draft
carried `sw:emission`, `sw:finalised` and `sw:completedEndpoint` "as predicates on the
endpoint". That puts triples in `current` that **no run graph contains**, which breaks this
plan's own constraint that `current` is reconstructible from the run graphs, and it forces
the CONSTRUCT to choose between publishing `<endpoint> sw:finalised true`, which is a wrong
fact because finalisation is a property of a run and not of an endpoint, and dropping the
inputs the unfinished-run sentences derive from. That is the C5 divergence reappearing inside
the fix for C5. So `current` names the run and the readers join to that run's graph for its
run-level facts: one hop, no copies, nothing in `current` that a run does not state.

**Why there are TWO pointers, which is the subtler half.** Stage 3-1's Critical was that the
newest run which **measured** an endpoint and the newest run which **sampled** it are
different runs the moment a cheap sweep declines `classes`, and that is the intended steady
state, not an edge case: the registry sweep declined `classes` for all 543 endpoints. A
single pointer with a single notion of recency therefore **loses the class sample outright**
once `endpoint_content.rq` reads `current`, silently undoing the fix stage 3-1 exists for.
`sw:currentSampleRun` carries the sample's own run, and the page keeps saying which sweep the
sample came from and that it was a different one. A test must pin exactly that: a store whose
newest run declined `classes` and whose older run sampled them still shows the sample,
attributed to the older sweep.

**The update rule, stated so the monotone trap cannot recur.** For every endpoint mentioned in the incoming run, replace what `current` holds for it, in **one `store.update()`** so the endpoint is never half-updated. Refuse to advance only when the run `current` already points at is **strictly newer** than the incoming one, so a re-load of the same run IRI does refresh (which is the recovery path, and under a strictly-greater test an out-of-order older run is still correctly refused).

**Three cases the rule cannot fix, which must be detected rather than left to a reader.** A run graph that shrinks (load the full sweep, then the truncated file a crashed prober left, which `run-truncated.nq` is a committed fixture of) leaves `current` attributing facts to a run that no longer states them. A dropped run graph does the same, and the spec's whole reason for one graph per run is that a bad run can be dropped wholesale. And the finished/unfinished flip must refresh. The loader detects the first two, by noticing an endpoint whose `sw:currentRun` names a run whose graph no longer mentions it, and says to rebuild.

**Substitution reaches updates**, which the reviewer verified, so the endpoint does not have
to be interpolated into the update string.

**Where the tied-run refusal goes.** The readers refuse a store with two runs tied as most
recent, and a "strictly newer" rule silently resolves such a tie by load order instead. That
is a behaviour change and it must be deliberate: **the loader refuses to advance `current`
past a tie it cannot break**, naming both runs, so the condition is still reported rather
than resolved by whichever file happened to be loaded second. Add a test.

**The migration guard.** `_opened_store` already refuses a store that is missing or empty, with a paragraph on why answering "nothing measured" out of an empty store is the failure to prevent. That reasoning applies verbatim: refuse a store that holds run graphs and no `current`, and name the rebuild command in the message.

**Correct `load_run.py:30-38` in this task.** It claims pyoxigraph 0.5.9 has no transaction API. `Store.update()` is documented as transactional and verified to hold across `;`. State what is actually true: there is no explicit transaction handle, `update()` gives transactional semantics, and for the run-graph replacement the practical objection is the size of an `INSERT DATA` body rather than the absence of a transaction.

- [ ] **Step 1: Move the fixtures onto `load_run` first, and watch the suite stay green**

`web/tests/conftest.py` and roughly ten inline `Store(str(tmp_path / "s"))` builds. Nothing else changes in this step. If the suite goes red here, that is a fixture that depended on a raw load and it needs understanding before anything else happens.

- [ ] **Step 2: Write the failing tests**

```python
def test_current_holds_no_typed_activity():
    # The C1 guard, first because it is the one that 500s every page.
    # Assert no `?a rdf:type prov:Activity` quad exists in the current graph.

def test_the_three_readers_return_what_they_returned_before_current_existed():
    # Golden: capture each reader's output on a load_run-built store BEFORE the
    # queries move, freeze it, and assert it after. The suite could not see C1
    # at all, so this is the only thing standing between a green commit and
    # every page 500ing.

def test_current_holds_the_newest_runs_facts_for_each_endpoint():
    # Two runs where a verdict CHANGES, so counting rows cannot pass it.

def test_an_endpoint_only_in_the_older_run_keeps_its_facts():
    # Why current is per-endpoint: a sweep that never reached an endpoint must
    # not erase what the last one learned.

def test_reloading_the_same_run_refreshes_current():
    # The monotone trap. Under "strictly newer" this must still update, because
    # re-loading is the documented recovery.

def test_loading_an_older_run_does_not_move_current_backwards():
    # The other direction.

def test_a_run_graph_that_shrinks_is_detected_and_named():
    # Load the full sweep, re-load the truncated file, and assert the loader
    # says which endpoints current now attributes to a run that dropped them.

def test_a_dropped_run_graph_is_detected():
    # remove_graph is used in the suite today and the spec calls dropping a bad
    # run a feature.

def test_the_unfinished_flip_refreshes():
    # Truncated run then the complete one: the page must stop saying the sweep
    # did not finish.

def test_a_rebuild_from_the_run_graphs_alone_reproduces_current():
    # Mutate current by hand, rebuild, compare.

def test_a_store_with_run_graphs_and_no_current_is_refused_at_open():
    # The migration guard, naming the rebuild command.

def test_an_empty_database_is_still_refused():
    # The spec names this case and the first draft dropped it.
```

- [ ] **Step 3: Run them, watch them fail, record what each printed**
- [ ] **Step 4: Implement, moving all three queries in this commit**
- [ ] **Step 5: Specify and test the check and the rebuild**

The check compares `current` against what the run graphs say and **names every endpoint that
drifted**, because a derived graph that cannot be verified is a liability and the index will
assert things a reader cannot cross-check by hand. The rebuild derives `current` from the run
graphs alone. **State the rebuild's algorithm**: the naive one-SPARQL-update shape measured
43.5 s at 30 runs, so the repair path must either accept that cost explicitly or work
endpoint by endpoint the way the maintenance path does. Report which, and its cost. Both
modes get a test, which an earlier draft promised and then dropped from its own Done-when.

- [ ] **Step 6: Measure, against the tables above**

Re-time all three queries at 1, 7 and 30 runs, and report the rebuild cost. **State every number.** If the page is not dramatically faster the stage has failed and the rest of the plan rests on it.

- [ ] **Step 7: Commit**

---

### Task 2: The index

**Files:**
- Create: `web/queries/index.rq`, `web/endpoint_index.py`, `web/templates/index.html`
- Modify: `web/app.py`
- Test: `web/tests/`

**What a row says.** The endpoint, and its verdict per metric drawn from `verdict_encoding.py`. **Render what the store holds:** 482 endpoints carry `availability` `indeterminate` and **4 carry `absent`**, and both are drawn as themselves. `absent` is common on other metrics too (`cors` 19, `cors-preflight` 78, `geo-data` 52, `service-description` 9), so a non-responder's row is routinely a mix.

**Do not assume a fixed metric set.** The metric list comes from the definitions a run recorded, not from a hard-coded eight, or the page breaks the first time `metrics.toml` changes.

**Grouping, and why not a boolean.** Group by the `availability` verdict's **own value**, one group per value present, each labelled with that state's own label from `verdict_encoding.py`, ordered by the encoding table's order rather than by any notion of better. Then a final group for endpoints whose newest run recorded no `availability` verdict at all, labelled "not measured" and merged with nothing: `run-prober-failed.nq` is a committed fixture of exactly that, and stage 1c-b3 makes it the normal outcome for a panicked host group. State each group's count **with its denominator**, as stage 1d-a's correction requires.

A heading like "did not answer" is forbidden: `absent` means the host answered with something that was not a SPARQL result, `indeterminate` covers a timeout, a DNS failure or an HTML front end, and collapsing them turns "we could not determine this" into a determined negative.

**Qualify a row whose facts are not current.** The endpoint page says when the run it shows did not finish, and when a newer unfinished run never reached this endpoint. The index shows 543 rows of exactly those facts, so it needs the same qualification or it silently presents stale rows as current. Note the second condition is store-wide, not per-endpoint, and the newest-activity aggregate that answers it measured **0 ms**, so it stays a query over the run graphs.

**No pagination**, because with `current` the query is a flat scan at 3.1 ms.

**The page-size budget is 500 KB of HTML, set here rather than after seeing the number.**
543 rows times the metric count plus the legend is roughly 0.5 to 1.2 MB on a plausible
markup budget, so this may well be exceeded. If it is, the remedy is **less markup per row**,
and the design already names the form: `Main.dc.html` renders capability chips as `c.abbr`,
an abbreviation, not a full label. Only if abbreviated chips still exceed the budget does
anything else get reconsidered, and then it comes back to me rather than being decided in the
task. Report the measured size either way.

**The index negotiates**, because the spec requires it of every resource.

**The filter script is inline, and that may not survive deployment.** The spec's risk table
flags the ids3 content security policy for the editor bundle, and an inline `<script>` is the
same question one stage early: it may need a nonce or to become a static file. One sentence
in the README, and do not let the page depend on the script running: with JavaScript off the
rows are all present and only the filter is missing.

- [ ] **Step 1: Write the failing tests**

```python
def test_the_index_lists_every_endpoint_current_knows():
    # Which fixture: derive a small one from the preserved run rather than
    # committing 6.3 MB, and document its provenance in test_fixture.py's list
    # the way every other synthetic fixture is. `~/code/sparqlwatch-runs/README.md`
    # keeps the real runs out of git deliberately. State the endpoint count the
    # fixture actually has rather than asserting 543 against something smaller.

def test_each_chip_matches_the_stored_verdict_for_that_endpoint_and_metric():
    # Two named endpoints, one indeterminate and one of the four absent, so a
    # substitute verdict cannot pass.

def test_no_row_displays_a_verdict_the_store_does_not_hold():

def test_groups_are_the_availability_values_present_in_encoding_order():
    # Assert the order and the labels, not the membership.

def test_an_endpoint_with_no_availability_verdict_gets_its_own_group():
    # From run-prober-failed.nq.

def test_each_group_states_its_count_with_the_denominator():

def test_a_row_whose_run_did_not_finish_is_qualified():

def test_a_row_links_to_the_percent_encoded_endpoint_page():

def test_the_index_and_the_endpoint_page_agree_about_a_verdict():

def test_the_index_negotiates_like_every_other_resource():

def test_the_chips_come_from_the_one_encoding_table():
```

- [ ] **Step 2: Run them, watch them fail, record what each printed**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Serve it against the real 543-endpoint store and look at it**

Report the page size, the render time, and the first rows of each group.

- [ ] **Step 5: Commit**

---

### Task 3: The exclusion mechanism

**Files:**
- Create: `prober/registry/exclusions.toml`
- Modify: `prober/src/registry.rs`, `prober/src/seed.rs`, `prober/README.md`
- Test: `prober/src/registry.rs`, `prober/src/seed.rs`

**This task exists because `/about` cannot be written honestly without it.** The reviewer grepped the whole repository: there is no contact address, no exclusion list, no opt-out and no removal path. The registry is regenerated from the LOD Cloud dump by `seed-registry`, so a manual deletion is undone by the next seed. A page that tells a sysadmin how to be excluded, when the next seed re-adds them, publishes a promise this project cannot keep, to hosts it has already contacted. **This project's rule about confident wrong answers covers claims about itself.**

An excluded URL must never reach a sweep, and a re-seed must preserve the exclusions. Put the rule where the other list-policy rules live, beside `without_credentials` and `without_reserved_names`, so there is one implementation of endpoint-list policy.

- [ ] **Step 1: Write the failing tests** (an excluded URL is absent from `load_endpoints`; absent from a fresh seed; the file's own reason field is required so an exclusion cannot be anonymous; a re-seed preserves the file)
- [ ] **Step 2: Run them, watch them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Commit**

---

### Task 4: `/about`

**Files:**
- Create: `web/templates/about.html`
- Modify: `web/app.py`
- Test: `web/tests/`

**This page has an audience and it is not us.** The prober's User-Agent points every request at `/about`, so this is the page a sysadmin reaches after finding an unfamiliar agent in their logs.

**Correct one thing the first draft said:** that URL does not 404 today, it is **unreachable**, because the host is not deployed until stage 4. So the promise is currently unkept rather than broken, and the page should be truthful about what the reader can do now.

**`/about` must not take the store dependency.** `get_store` raises on a missing or empty
store, and this is the one page a stranger reaches; it has to answer when the store does not.
Add a test that it responds with no store configured at all.

It must answer, plainly and near the top: what this is; that it probes public SPARQL endpoints and publishes what it measured; **how often and how politely**, taking the numbers from `main.rs`'s actual defaults rather than a remembered pair; where the endpoint list came from; and **how to ask to be excluded**, describing the mechanism Task 3 builds and saying plainly that requests are handled by hand today.

**The contact address is `michel.dumontier@maastrichtuniversity.nl`**, supplied for this
purpose. Name it, and say plainly that requests are read and handled by a person rather than
by an automated system, because that is what is true: Task 3 builds the list an exclusion
lands in, and nothing watches a mailbox. Do not soften that into an implied service level.

This is a public page, so the address is published deliberately and not as a side effect.
Do not add any other address, and do not invent a form, a ticket queue or an alias.

Say what the site cannot yet do, for the same reason the README does.

- [ ] **Step 1: Write the failing tests** (it responds; it negotiates; it states the politeness defaults and they match `main.rs`; it describes the exclusion mechanism that now exists; it does not name a channel that does not)
- [ ] **Step 2: Run them, watch them fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Read it as a stranger would, then commit**

---

### Task 5: Document it, and correct what the measurement disproved

**Files:**
- Modify: `web/README.md`, `prober/README.md`, spec

- [ ] **Step 1: The READMEs**

The routes; `current` and why it is load-bearing rather than an optimisation; its rebuild and check modes; the migration pass an existing store needs; and the before-and-after timing tables, which are the justification for the whole stage.

**Correct the transaction claim wherever it appears**, not only in `load_run.py`. It has been
shaping design decisions since stage 2-1.

**Reconcile, do not append.** `web/README.md:354-358` already carries newest-run-branch
timings at 402 and 1002 runs. Those describe the same query this stage rewrites, so the new
table replaces or explains them rather than sitting beside them saying something different
about the same thing. Note also that those figures were taken on small runs, which is why
they look fast: the cost tracks quad count, not graph count, and that is worth saying because
it is why a measurement that looked reassuring did not catch a 5.8-second page.

**State the deferral honestly:** if the content sample has not moved to `current`, the RDF representation stays O(history) and one expensive sweep makes it the slowest thing on the site.

- [ ] **Step 2: The spec**

Stage 3's row moves on: the index and `/about` exist. Record the facets as **blocked on stage 2b** rather than merely unbuilt.

Correct `:113-115` in two ways, and say why: the derived graph is **per endpoint**, not per (endpoint, metric), because mixing metrics from different runs is the defect stage 3-1 was fixed for; and it is **maintained incrementally**, not rebuilt after each run, because a rebuild measured 43.5 s at 30 runs. The spec predicted the graph; the code's reasoning about its granularity is better than the spec's and the spec is what changes.

- [ ] **Step 3: Commit**

---

## Done when

- `current` holds no typed activity, and a test asserts it.
- A golden test proves all three readers return what they returned before `current` existed, on stores built through `load_run`.
- The fixtures go through `load_run`, so the suite can see the graph at all.
- Re-loading the same run refreshes; an older run does not move it backwards; a shrunken or dropped run graph is detected and the affected endpoints are named; the unfinished flip refreshes.
- Every `current` update is one atomic `store.update()`, so no endpoint is ever half-updated.
- A store with run graphs and no `current` is refused at open, naming the rebuild command, and an empty database is still refused.
- All three read paths read `current`, moved in one commit, with before-and-after timings at 1, 7 and 30 runs recorded.
- Every chip matches the stored verdict for that endpoint and metric, including the four `absent` ones, and no row shows a verdict the store does not hold.
- Groups are the `availability` values present, in encoding order, with their own labels, plus a "not measured" group for endpoints with no such verdict, each count carrying its denominator.
- A row whose facts are not current is qualified the way the endpoint page qualifies them.
- The index negotiates.
- An excluded URL reaches no sweep and survives a re-seed.
- `/about` states what is true today, including that requests are handled by hand, and names no channel that does not exist.
- The transaction claim is corrected everywhere it appears, and the spec records `current` as per-endpoint and incrementally maintained, with the reasons.
