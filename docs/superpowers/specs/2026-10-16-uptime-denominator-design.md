# Uptime's missing denominator

**Status:** proposal, not approved. Nothing here is implemented.

## The problem

`sparql.healthdata.nl` answered one probe in thirty-four on 2026-10-15. Its
page reports **100% uptime** for that day.

| day | verified | indeterminate | probes sent | the page says |
|---|---|---|---|---|
| 2026-10-16 | 3 | 8 | 11 | 100% of 3 sweeps |
| 2026-10-15 | 1 | 33 | 34 | **100% of 1 sweep** |
| 2026-10-14 | 0 | 48 | 48 | 0% of 48 sweeps |
| 2026-10-13 | 2 | 28 | 30 | 100% of 2 sweeps |
| 2026-10-12 | 6 | 17 | 23 | 100% of 6 sweeps |

The number is not a bug in its own terms. `_daily_series` computes it as

```python
answered = [r for r in asked if r.verdict != "indeterminate"]
counted  = answered or asked
up       = [r for r in counted if r.verdict in _POSITIVE_VERDICTS]
uptime   = 100 * len(up) / len(counted)
```

— "of the probes that produced an answer, this share were good." On 10-15 that
is one probe, and it was good, so 100% is arithmetically correct and
substantively misleading. The `sweeps` count beside it does say `1`, which is a
partial admission nobody reads next to a round number.

Note the shape of the error: it is a **cliff, not a slope**. One success in
thirty-four reads 100%; zero successes in forty-eight reads 0%, because
`answered or asked` falls back when nothing answered at all. There is no middle,
so the chart is flat at the top until the day it drops to the floor.

## Why the rule exists, and why it should survive

Excluding `indeterminate` fixed a real defect and the reasoning is recorded at
`app.py:1582`. The verdict means no answer arrived — a throttle, a timeout, a
proxy, a blip on our side — and `resolve.rs` calls that *"we never got to ask"*,
deliberately not *"it does not answer"*. Counting it as downtime once published
reduced uptime for endpoints that were fine: on 2026-09-27 this service showed
Wikidata, NLM MeSH and three IDSM endpoints below full availability while all
five answered its own liveness query in under a second.

**Any change here must not reintroduce that.** Publishing a confident negative
about a stranger's server is the one failure this codebase is arranged to
avoid, and it is worse than publishing an uninformative positive.

## What the rule conflates

`indeterminate` is one verdict covering two different facts:

1. **We could not ask.** Our metric budget expired, our socket was stale, our
   proxy failed. Rightly excluded — it is a fact about us.
2. **We asked and nothing came back.** The tunnel was established, the request
   went out, and the server said nothing for thirty seconds. That is a fact
   about the endpoint, and it is currently excluded too.

healthdata is case 2, and we have independent proof: the egress proxy logged
`TCP_TUNNEL/200 4217 bytes 29992ms` on every failure against `47ms 5348 bytes`
for a working request — byte-identical each time, enough for a handshake and our
request, with the response missing.

## What we already have

**No new data is needed for the presentation fix.** `_daily_series` computes
`asked` and `answered` and throws the difference away. For closed days,
`daily.py` deliberately stores **counts per verdict** rather than a decided
up/down ("COUNTS PER VERDICT, NOT UP AND DOWN", `daily.py:11`), so
`cell["verdicts"]` already carries the `indeterminate` count. Summarised and
raw days can both report it with no migration and no change to the published
RDF.

## Proposal

### Stage 1 — show the denominator (recommended, no vocabulary change)

Keep the uptime number exactly as defined. Stop hiding how many probes produced
nothing.

- `_daily_series` and `_day_from_counts` each gain `unanswered` (the
  `indeterminate` count) and `probes` (the total asked) beside the existing
  `sweeps`.
- `charts._readout` names it in the hover sentence.
- The accessible table gains a column.
- The chart geometry does **not** change in this stage. A third visual state on
  the bar is a real encoding decision with palette and legend consequences; it
  belongs in its own pass, after the words are right.

This publishes no new claim about anybody's server. It reports what we did and
what came back, and lets a reader draw the conclusion.

**Wording needs explicit approval.** Three candidates for the hover sentence,
currently `2026-10-15 — 100% of 1 sweep, 137 ms median, 149 ms p95`:

- **A** `2026-10-15 — 34 probes, 1 answered, 100% of those verified; 137 ms median, 149 ms p95`
- **B** `2026-10-15 — 100% of 1 answered probe; 33 of 34 got no answer; 137 ms median, 149 ms p95`
- **C** `2026-10-15 — 1 of 34 probes answered, and it verified; 137 ms median, 149 ms p95`

A leads with what we did. B keeps the current sentence's shape and appends. C
reads most like a sentence and drops the percentage from the hover entirely.
The table column name needs picking too: `no answer`, `unanswered`, or
`silent`.

### Stage 2 — attribute the silence (deferred, and possibly never)

To separate case 1 from case 2 in the data, the failure class would have to
reach the run graph. `why_indeterminate` already derives it but only logs it,
and `reqwest::Error` exposes it as typed predicates (`is_timeout`,
`is_connect`, `is_body`, `is_request`) rather than strings to parse. The
vocabulary has precedent for this shape in `sw:notMeasuredReason`
(`cost-ceiling`, `prober-failed`, `liveness-failed`), so a sibling
`sw:indeterminateBecause` on the measurement would follow an established
pattern rather than invent one.

**I am not proposing we then count it as downtime**, and this is the important
part. Through a forward proxy, a timeout after a successful connect *to the
proxy* cannot be attributed from inside the client: the proxy failing to reach
the origin and the origin failing to answer look identical. Only the proxy's own
log distinguishes them, and the prober does not read it. So even with the reason
emitted, "they did not answer" would sometimes be our fault — exactly the error
of 2026-09-27, in a new costume.

Stage 2's honest value is **operational**, for our own debugging, not public
scoring. It should be judged on that, separately, and it is not required for
Stage 1.

## What does not change

- The `indeterminate` verdict and the six-verdict vocabulary.
- The published RDF, in Stage 1.
- The uptime formula, in both stages.
- Response time staying conditional on success — a timed-out probe records its
  full 30,000 ms budget, and admitting those to the median would report as
  "typical" a duration no working request ever took.
- A day with no sweeps staying a gap rather than a zero.

## Tests

- A day of 1 verified and 33 indeterminate reports 100% **and** 33 unanswered
  of 34 — the healthdata case, as a fixture.
- A summarised day and a raw day with identical verdict counts produce identical
  output, which `daily.agrees_with_raw` already gates.
- A day of 0 verified and 48 indeterminate still reports 0% of 48, so the
  `answered or asked` fallback is not disturbed.
- A day with no sweeps still reports a gap, not 0%.
- The 2026-09-27 regression: an endpoint with occasional indeterminate probes
  and otherwise verified ones keeps 100% uptime, since nothing here counts
  silence as downtime.

## Open questions

1. Which wording, and which column name.
2. Whether the chart should eventually show unanswered probes as a visual
   state, or whether the readout and table are enough.
3. Whether Stage 2 is worth doing for operational reasons alone.
