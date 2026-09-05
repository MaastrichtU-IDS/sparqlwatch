use crate::budget::Expired;
use crate::client::ORIGIN;
use crate::declare::Declarations;
use crate::metrics::{MetricDef, ProbeKind};
use crate::observe::{BodyKind, Observation};
use crate::verdict::{Level, Verdict};

/// What the endpoint says about itself, from its service description.
///
/// `claimed: false` covers two situations this type cannot tell apart on its
/// own: the description genuinely declares nothing, or we never managed to
/// read one at all (no fetch reached it, the fetch found an empty body, or
/// the body failed to parse). The two are distinguished by the endpoint's
/// `declarationsRead` fact, one boolean published per endpoint per run (see
/// `emit::DeclarationsRead`), computed in `run_sweep` as
/// `declarations.triples > 0`. A partially parsed description is the
/// concrete case where the distinction matters: `declare.rs` keeps whatever
/// declarations it read before a syntax error, so `claimed` can be `true`
/// there while `service-description`'s own row grades `Indeterminate`
/// because the body never classified as RDF.
#[derive(Debug, Clone, Copy)]
pub struct Declared {
    pub claimed: bool,
    /// The NUMBER the description stated, for a metric that compares one.
    ///
    /// `None` for every other kind, and `None` also when a counting metric's
    /// description stated nothing. Those two are the same thing here: no claim
    /// was made, so there is nothing to be right or wrong about.
    pub value: Option<u64>,
}

impl Declared {
    /// `claimed` is true only when `def` names a declaration that could
    /// speak for it (`declared_by: Some(iri)`) and that declaration actually
    /// appears in the fetched description. A metric with no `declared_by` --
    /// liveness, response time, CORS headers -- is never "claimed": there is
    /// no declaration in the vocabulary that could confirm or deny it.
    pub fn from(defs: &Declarations, def: &MetricDef) -> Declared {
        // A counting metric's `declared_by` names a predicate carrying a
        // NUMBER, so it is read for its value and `claimed` follows from
        // whether that value is there. Every other kind asks `declares`,
        // which answers whether the capability was mentioned at all.
        if def.kind == ProbeKind::Counted {
            let value = def.declared_by.as_deref().and_then(|iri| defs.count_of(iri));
            return Declared { claimed: value.is_some(), value };
        }
        Declared {
            claimed: def.declared_by.as_deref().is_some_and(|iri| defs.declares(iri)),
            value: None,
        }
    }
}

/// Whether the request reached the endpoint and got back a successful HTTP
/// status. Used to tell "the endpoint told us it has nothing" apart from
/// "something upstream (a proxy, an outage) prevented us from finding out":
/// only the former is safe to report as `Absent`.
fn answered_ok(o: &Observation) -> bool {
    matches!(o.status, Some(s) if (200..=299).contains(&s))
}

/// Whether a declared count and a counted one agree.
///
/// The comparison is RELATIVE, not absolute: a thousand triples out of a
/// million is noise and a thousand out of two thousand is a different dataset.
/// `None` demands exact equality.
///
/// Zero declared and zero counted agree. Zero declared against anything else
/// does not, whatever the tolerance, because a fraction of zero is zero: an
/// endpoint that said it holds nothing and holds a million is wrong by any
/// reading, and the relative test alone would divide by it.
fn within_tolerance(stated: u64, found: u64, tolerance: Option<f64>) -> bool {
    if stated == found {
        return true;
    }
    let Some(tolerance) = tolerance else { return false };
    if stated == 0 {
        return false;
    }
    let drift = (found as f64 - stated as f64).abs() / stated as f64;
    drift <= tolerance
}

/// Whether an observation is a SPARQL result set this prober may read, empty
/// or not.
///
/// The gate the `SelectIris` arm below applies before it will treat bindings
/// as evidence, named and made public so the class profile pass applies the
/// same one. Two probes reading the same evidence shape must decide the same
/// way about it, and the pass has to make this call outside `resolve()`
/// because a `ClassProfile` metric publishes no verdict for `resolve()` to
/// produce.
///
/// A 500 carrying a populated `results.bindings` fails this: the engine
/// errored, so its body is not an answer, however well-formed.
pub fn is_a_readable_result(o: &Observation) -> bool {
    o.body_kind == BodyKind::SparqlJson && answered_ok(o)
}

/// The verdict for a probe that CONFIRMED the capability, and the only place
/// that decision is made. Every arm below routes its positive outcome through
/// here, so the declared/observed axis reads the same way on every published
/// row.
///
/// The rule: the axis applies only where a declaration is possible. A metric
/// carrying no `declared_by` names nothing in the service-description
/// vocabulary that could ever speak for it (liveness, response time, CORS
/// headers, class counts), so "undeclared" says nothing about the endpoint and
/// the confirmation stands on its own as `Verified`. A metric that does carry
/// one is the case the second verdict exists for: `geo-functions` is the only
/// shipped example, and it carries this project's headline finding, that 18
/// surveyed endpoints evaluate `geof:sfWithin` and none of them declares it.
///
/// This REVERSES commit d4ff4f4, which read `verified` as "confirmed AND
/// declared" and moved the `CorsPreflight` arm to `UndeclaredButVerified` on
/// that reading. Publishing "works, but advertises nothing" about a capability
/// no vocabulary term can advertise is a category error, and it dilutes the one
/// verdict where the distinction carries a finding. If a declaration for CORS
/// (or for liveness, or for class counts) ever enters the vocabulary, adding
/// `declared_by` to that metric upgrades it here with no code change.
fn confirmed(def: &MetricDef, declared: Declared) -> Verdict {
    if def.declared_by.is_none() || declared.claimed {
        Verdict::Verified
    } else {
        Verdict::UndeclaredButVerified
    }
}

/// Whether `access-control-allow-methods` permits the GET we would send. An
/// absent header is a grant: the header is optional and a preflight that
/// answered without it refused nothing. An empty header is NOT a grant, because
/// the server stated a list and GET is not in it.
///
/// Comparison is per comma-separated entry, never a substring search: a list of
/// `POSTGET` (or, in the wild, a header value mangled by a proxy) contains the
/// three letters of GET and permits nothing.
fn allows_get(allow_methods: Option<&str>) -> bool {
    let Some(list) = allow_methods else {
        return true;
    };
    if list.trim() == "*" {
        return true;
    }
    list.split(',').any(|m| m.trim().eq_ignore_ascii_case("GET"))
}

/// Whether `access-control-allow-origin` grants OUR origin. A wildcard grants
/// everyone; an exact echo of our origin grants us. Anything else is a grant to
/// somebody else, and reporting it as ours would publish `verified` for an
/// endpoint that would refuse us in a browser.
///
/// `None` (no header at all) is not a grant, which is why the probe records the
/// header's value and not merely `Observation.cors`.
fn grants_our_origin(allow_origin: Option<&str>) -> bool {
    let Some(value) = allow_origin else {
        return false;
    };
    let value = value.trim();
    value == "*" || value.eq_ignore_ascii_case(ORIGIN)
}

/// Turn observation plus declaration into a verdict. This is the only place
/// judgement happens: no probing, no I/O, pure function.
///
/// The rule applied uniformly below: **an absence claim requires the endpoint
/// to have answered the question we asked.** `Absent` (and
/// `DeclaredButWrong`, which the spec ranks as worse still) may only be minted
/// from a response the endpoint itself authored. For most probe kinds that
/// means `answered_ok`: a 2xx, in a form we could read. Anything else -- a
/// throttle, a gateway error, an unparseable body, a response we never got --
/// is `Indeterminate`.
///
/// Two deliberate departures, both because the status IS the answer for the
/// question being asked:
///
/// - the `Cors` *positive* case, because an `access-control-allow-origin`
///   header proves CORS is configured at any status;
/// - `CorsPreflight`, where a `405` or a `501` is the endpoint telling us it
///   will not serve a browser's preflight, so those statuses are an absence
///   rather than an unknown. A `3xx` is not: `Client::preflight` resolves the
///   redirect chain and this verdict is drawn from its end, so a `3xx` here
///   means we never reached an answer. `resolve_fetch` has the third such
///   case, a `404`/`410` on the description.
pub fn resolve(def: &MetricDef, declared: Declared, obs: Result<&Observation, Expired>) -> Verdict {
    let o = match obs {
        Err(Expired) => return Verdict::Indeterminate,
        Ok(o) => o,
    };

    // A transport failure or an HTML console tells us nothing about the
    // attribute itself.
    if o.error.is_some() || o.body_kind == BodyKind::Html {
        return Verdict::Indeterminate;
    }

    match def.kind {
        // A profile is not a measurement, so it has no verdict to resolve. This
        // arm exists to be unreachable rather than to compute anything: nothing
        // routes a ClassProfile metric through here, because `probe_endpoint`
        // handles it after the per-metric dispatch and pushes no MeasurementRow.
        // Ruling 2 in
        // docs/superpowers/specs/2026-08-29-content-profiles-design.md.
        //
        // `Indeterminate` and not a panic, because a wrong answer here would be
        // a published verdict on somebody's endpoint and this is the one value
        // that claims nothing. If it ever appears in a run graph, the routing is
        // broken and the graph says so honestly.
        ProbeKind::ClassProfile => Verdict::Indeterminate,
        ProbeKind::AskFilter | ProbeKind::AskData => match (o.boolean, def.expect) {
            // This arm serves both probe kinds, and the two readings differ:
            // for `AskFilter` a wrong boolean means broken semantics; for
            // `AskData` a `false` means the data is absent. It is safe today
            // only because no shipped `AskData` metric sets `expect`, and the
            // day one does, genuine data absence would silently become
            // `DeclaredButWrong` instead of `Absent`.
            // A capability claim needs the endpoint's own successful answer just
            // as much as an absence claim does: a 500 body that happens to carry
            // {"boolean": true} is not the engine confirming anything.
            (Some(got), Some(want)) if got == want => {
                if answered_ok(o) { confirmed(def, declared) } else { Verdict::Indeterminate }
            }
            // Bound but wrong: the function answered, and answered
            // incorrectly. Only claimable when the endpoint itself answered
            // with a success status; a 502 body that happens to parse is not
            // the engine's answer.
            (Some(_), Some(_)) => {
                if answered_ok(o) { Verdict::DeclaredButWrong } else { Verdict::Indeterminate }
            }
            (Some(true), None) => {
                if answered_ok(o) { confirmed(def, declared) } else { Verdict::Indeterminate }
            }
            (Some(false), None) => {
                if answered_ok(o) { Verdict::Absent } else { Verdict::Indeterminate }
            }
            (None, _) => {
                if declared.claimed { Verdict::DeclaredOnly } else { Verdict::Indeterminate }
            }
        },
        ProbeKind::Cors => {
            if o.cors {
                // The header proves CORS is configured whatever the status
                // code, so the positive case is not gated on `answered_ok`.
                confirmed(def, declared)
            } else if answered_ok(o) {
                Verdict::Absent
            } else {
                // A proxy-generated error status cannot be attributed to the
                // endpoint's own CORS configuration.
                Verdict::Indeterminate
            }
        }
        ProbeKind::CorsPreflight => {
            // ORDER IS PART OF THE SPECIFICATION HERE, and the status gate
            // comes FIRST. An earlier draft of these rules was a table, the
            // natural implementation checked the headers before the status,
            // and it returned `Verified` for a `405` carrying
            // `access-control-allow-origin: *` -- a blanket header from a
            // front-end filter over a handler that refuses OPTIONS, which is
            // exactly the endpoint this metric exists to catch. Do not hoist
            // the header check above this match.
            //
            // Rule 1 (expired budget, transport error) is handled by the
            // guards at the top of this function.
            match o.status {
                // Fetch requires the preflight to answer with an ok status, so
                // a 405/501 fails the preflight whatever headers ride along.
                // This is the endpoint answering the question we asked, which
                // is what licenses an absence claim.
                Some(405) | Some(501) => Verdict::Absent,
                // A 3xx that survives as far as this rule is a chain
                // `Client::preflight` could not resolve: no usable `Location`,
                // a cycle, or more hops than its bound. A resolvable redirect
                // never arrives here, because that client re-issues the
                // `OPTIONS` at the target and this verdict is drawn from the
                // response at the end of the chain.
                //
                // It was `Absent` once, on the reasoning that a browser fails
                // a redirected preflight. True of the browser, but wrong about
                // the endpoint: every other probe reaches the endpoint through
                // its redirect, so one run published `cors =
                // undeclared-but-verified` and `cors-preflight = absent` for
                // one service, and the contradiction came from our redirect
                // policy rather than from anything the endpoint did. An
                // unresolvable chain means we never got a preflight answer,
                // which is exactly what `Indeterminate` says.
                Some(s) if (300..=399).contains(&s) => Verdict::Indeterminate,
                // Any other non-2xx describes our request or the server's
                // state, not its CORS policy.
                Some(s) if !(200..=299).contains(&s) => Verdict::Indeterminate,
                // 2xx: now, and only now, the headers decide.
                Some(_) => {
                    if grants_our_origin(o.allow_origin.as_deref()) && allows_get(o.allow_methods.as_deref()) {
                        // Through the same helper as every other arm, so the
                        // two CORS rows and every other confirmation read
                        // alike. `cors-preflight` carries no `declared_by`, so
                        // today this is `Verified`.
                        confirmed(def, declared)
                    } else {
                        // The endpoint answered the preflight and did not grant
                        // us the request we would make.
                        Verdict::Absent
                    }
                }
                // No status and no error is not a state `Client::preflight` can
                // produce, but a status is the evidence every rule above rests
                // on, so its absence is an unknown rather than an absence.
                None => Verdict::Indeterminate,
            }
        }
        ProbeKind::Liveness => {
            // A SPARQL JSON body is proof it speaks the protocol. A non-SPARQL
            // body only establishes absence when the endpoint answered
            // successfully: a 429 with a plain-text body, a non-HTML 502/503
            // from an intermediary, or a 401/403 all mean we never got to ask
            // the question. This metric runs against every endpoint every
            // sweep and a later stage reads it to decide admission, so a
            // throttled endpoint must not be tombstoned as unreachable.
            if o.body_kind == BodyKind::SparqlJson {
                // Gated, like `AskData` and `SelectIris`, and for the reason
                // `SelectIris` already states: two metrics reading the same
                // evidence shape must apply the same rule to it. A 429 or a 503
                // carrying a SPARQL-results body is the endpoint refusing to
                // serve us, whatever its body parses as, and `verified` is an
                // assertive verdict that its own status contradicts.
                //
                // The safe direction here is `Indeterminate`, not `Absent`, so
                // this does NOT reintroduce the tombstoning the paragraph above
                // guards against: a throttled endpoint still reads "we never got
                // to ask", never "it does not answer".
                //
                // `Cors` remains the one deliberate exception in this function,
                // because an `access-control-allow-origin` header proves CORS is
                // configured whatever status carries it. A parsed body proves
                // only that something answered.
                if answered_ok(o) { confirmed(def, declared) } else { Verdict::Indeterminate }
            } else if answered_ok(o) {
                Verdict::Absent
            } else {
                Verdict::Indeterminate
            }
        }
        // The content verdict, and the only one there is.
        //
        // The "observation" here is synthesised by `probe_endpoint` from the
        // profile pass's own results: `bindings` holds the classes the pass
        // actually found, and the status says whether the pass ran at all.
        // Nothing is sent to the endpoint for it.
        //
        // `declared.claimed` is whether the description NAMED any classes,
        // through a `void:classPartition`. So the four states are the four
        // corners of declared against observed, which is the axis this project
        // exists to report, applied to what an endpoint holds:
        //
        //   declared, found     -> verified
        //   found, undeclared   -> undeclared-but-verified
        //   declared, not found -> declared-only
        //   neither             -> absent
        //
        // `declared-but-wrong` is deliberately NOT produced. It would mean the
        // description named classes and the endpoint holds none of them, and
        // this metric cannot tell that apart from a pass that reached a subset:
        // the enumeration is capped at 200 classes and the ladder samples, so a
        // named class missing from the profiles may simply not have been
        // reached. Reporting the harshest verdict in the vocabulary on that
        // evidence would be a false accusation.
        ProbeKind::VocabularyDescribed => {
            if !o.bindings.is_empty() {
                confirmed(def, declared)
            } else if is_a_readable_result(o) {
                // The pass ran and found no classes at all.
                if declared.claimed { Verdict::DeclaredOnly } else { Verdict::Absent }
            } else {
                // The pass did not run, or could not finish. Nothing is known.
                Verdict::Indeterminate
            }
        }
        // One number the endpoint states about itself, against the number we
        // counted. The declared value is what this service reports as the
        // endpoint's size; this says whether that statement is true.
        //
        // `declared-but-wrong` IS reachable here, unlike anywhere else in this
        // file, and it is the whole point: a description claiming ten times the
        // triples an endpoint holds is the most useful thing a monitor can tell
        // a consumer. It is gated on the metric's `tolerance` so that a
        // description written before the dataset grew is not called wrong for
        // being a few percent out.
        ProbeKind::Counted => {
            // A count we may believe, or none. `None` here is NOT zero, and
            // conflating the two was the first version of this arm: a COUNT
            // query with no GROUP BY returns exactly one row from any real
            // engine, so no row at all means we never learned the number,
            // while a row saying 0 means the endpoint told us it holds
            // nothing. Reading the first as the second publishes "this
            // endpoint is empty" about an endpoint that never answered.
            let counted = if is_a_readable_result(o) {
                o.bindings.first().and_then(|b| b.trim().parse::<u64>().ok())
            } else {
                None
            };
            match (declared.value, counted) {
                // Both in hand: the comparison this metric exists for. Two
                // zeros agree, which `within_tolerance` handles directly.
                (Some(stated), Some(found)) => {
                    if within_tolerance(stated, found, def.tolerance) {
                        Verdict::Verified
                    } else {
                        Verdict::DeclaredButWrong
                    }
                }
                // Counted zero and nothing declared. The endpoint answered and
                // told us it holds none, which is a finding rather than a gap.
                (None, Some(0)) => Verdict::Absent,
                // Counted, never declared. The endpoint holds this much and
                // says nothing about it.
                (None, Some(_)) => confirmed(def, declared),
                // Declared, and we could not count it. The claim stands
                // unchecked, which is exactly what `declared-only` means.
                (Some(_), None) => Verdict::DeclaredOnly,
                // Neither stated nor learned. Nothing is known, and saying
                // `absent` here would be the false negative this project
                // exists to prevent.
                (None, None) => Verdict::Indeterminate,
            }
        }
        ProbeKind::SelectIris => {
            if !o.bindings.is_empty() {
                // Gated like `AskData`'s positive case, and for the same
                // reason: a 500 body that happens to carry a populated
                // `results.bindings` is not the engine confirming anything.
                // Two metrics reading the same evidence shape must apply the
                // same rule to it.
                if answered_ok(o) { confirmed(def, declared) } else { Verdict::Indeterminate }
            } else if is_a_readable_result(o) {
                // Empty bindings are only evidence of absence when we
                // actually parsed a result.
                Verdict::Absent
            } else {
                Verdict::Indeterminate
            }
        }
        // `resolve()` only carries `declared.claimed`, not the fetched
        // `Declarations` itself, so it cannot grade here; it delegates to
        // `resolve_fetch` for the verdict and discards the level. `run_sweep`
        // calls `resolve_fetch` directly for the graded row, so the level is
        // never lost, only unavailable on this path.
        ProbeKind::FetchWellKnown => resolve_fetch(&Declarations::empty(), Ok(o)).0,
    }
}

/// Grade a fetched service description by what its own content declares,
/// mapping the fields straight onto `grade_service_description`.
pub fn grade_from_declarations(defs: &Declarations) -> Level {
    grade_service_description(
        defs.triples,
        defs.names_dataset,
        defs.has_void_partitions,
        defs.has_entailment,
        // The DOCUMENT-wide flag, never the scoped set: the grade describes
        // what the operator published, not what one service claims.
        defs.doc_declares_extension_functions,
        defs.has_example_resources,
    )
}

/// Resolve the `FetchWellKnown` probe: did the endpoint publish a
/// dereferenceable, parseable service description, and if so, how informative
/// is it. The fetch behind this is a queryless GET on the endpoint URL itself
/// (see `Client::fetch_rdf`), not a request to any `/.well-known/` path; the
/// probe-kind name is legacy from an earlier design and is kept for now, but
/// no code here dereferences a well-known URL.
///
/// `Absent` is minted only from a `404` or a `410`: those are the only status
/// codes that speak to what is published at the URL, and they are one of the
/// three places in this module where a non-2xx status licenses an absence,
/// alongside the `405`/`501` of a refused `CorsPreflight` -- nothing was ever
/// there, or it was and has since been removed. Every other non-2xx status,
/// `401`/`403` included, describes our request or the server's state, not
/// the endpoint's published metadata, so it stays `Indeterminate`. This is
/// deliberately narrow, not merely cautious: the survey behind this project
/// recorded 10 of 548 endpoints returning a plain `400` for a queryless
/// GET -- rejecting the very shape of request this probe makes -- plus 5
/// more unrelated `400`s, and a `429` throttle is exactly the situation
/// where we know least about what exists. Treating the wider `4xx` range as
/// `Absent` would have reported every one of those as "no service
/// description published," a false absence -- the failure mode this system
/// exists to prevent. A future reader tempted to widen this back to `4xx`
/// should re-read that number first.
pub fn resolve_fetch(defs: &Declarations, obs: Result<&Observation, Expired>) -> (Verdict, Option<Level>) {
    let o = match obs {
        Err(Expired) => return (Verdict::Indeterminate, None),
        Ok(o) => o,
    };

    if o.error.is_some() || o.body_kind == BodyKind::Html {
        return (Verdict::Indeterminate, None);
    }

    // A parseable RDF body is a published description only when the endpoint
    // answered with a 2xx. `verified` is as assertive a verdict as `absent`,
    // so it gets the same gate every other probe kind applies: a 429 throttle
    // notice, a 500 error document and a 503 maintenance page can all carry an
    // RDF-ish payload, and none of them is the endpoint publishing a service
    // description. A non-2xx falls through to the status match below, so 404
    // and 410 still resolve to a genuine absence and everything else stays
    // indeterminate.
    if o.body_kind == BodyKind::Rdf && answered_ok(o) {
        return (Verdict::Verified, Some(grade_from_declarations(defs)));
    }

    match o.status {
        Some(404) | Some(410) => (Verdict::Absent, Some(Level(0))),
        _ => (Verdict::Indeterminate, None),
    }
}

/// Grade a service description by what it actually tells a client, not by its
/// presence. 21 of 28 descriptions in the wild are the same 14-triple
/// Virtuoso stub, so a stub must not score the same as a real description.
///
/// Reached via `grade_from_declarations`, which `resolve_fetch` calls once a
/// fetch probe reports a parsed `BodyKind::Rdf` body from a 2xx. The grade is
/// carried out of `resolve_fetch` by `run_sweep` and lands on the
/// `MeasurementRow.level` of any metric its definition marks `graded`.
///
/// The ladder is the spec's (design doc line 433): level 4 is "declares an
/// entailment regime, example resources, or extension functions", so all
/// three reach it. It is monotonic by construction: every criterion is a
/// declaration the description either makes or does not, and adding one can
/// only move a description up the ladder, never down. That is what makes the
/// number readable as informativeness rather than as a taxonomy.
pub fn grade_service_description(
    triples: usize,
    names_dataset: bool,
    has_void_partitions: bool,
    has_entailment: bool,
    has_extension_functions: bool,
    has_example_resources: bool,
) -> Level {
    let n: u8 = if triples == 0 {
        0
    } else if has_entailment || has_extension_functions || has_example_resources {
        4
    } else if has_void_partitions {
        3
    } else if names_dataset {
        2
    } else {
        1
    };
    // Levels are born valid: constructing through `Level::new` rather than
    // the raw tuple keeps the 0..=4 bound enforced at the one place grades
    // are computed, even though the field itself is public.
    Level::new(n).expect("service-description grade is 0..=4 by construction")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Cost;

    /// The one IRI any shipped metric names in `declared_by`, and so the one
    /// capability the declared/observed axis can currently apply to.
    const SF_WITHIN: &str = "http://www.opengis.net/def/function/geosparql/sfWithin";

    fn def(kind: ProbeKind, expect: Option<bool>) -> MetricDef {
        MetricDef {
            id: "t".into(),
            label: "t".into(),
            dimension: "d".into(),
            kind,
            query: None,
            expect,
            var: None,
            declared_by: None,
            graded: false,
            cost: Cost::Cheap,
            sample_limit: None,
            sample_prefix: None, tolerance: None,
        }
    }

    fn obs(boolean: Option<bool>) -> Observation {
        Observation { status: Some(200), cors: true, boolean, bindings: vec![], body_kind: BodyKind::SparqlJson, body: None, final_url: None, content_type: None, allow_origin: None, allow_methods: None, allow_headers: None, profile: None, elapsed_ms: 5, error: None }
    }

    #[test]
    fn works_and_declared_is_verified() {
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: true, value: None }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn works_but_undeclared_is_its_own_verdict() {
        // The commonest real case: 18 endpoints evaluate geof:sfWithin and
        // none of them declares it. The metric has to carry a `declared_by`
        // for the verdict to mean anything: the declared/observed axis applies
        // only where a declaration was possible in the first place.
        let mut d = def(ProbeKind::AskFilter, Some(true));
        d.declared_by = Some(SF_WITHIN.into());
        let v = resolve(&d, Declared { claimed: false, value: None }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn wrong_answer_is_worse_than_absent() {
        // 9 endpoints answered `false` to a filter a conformant engine must
        // answer `true`: the function is bound but the semantics are wrong.
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false, value: None }, Ok(&obs(Some(false))));
        assert_eq!(v, Verdict::DeclaredButWrong);
    }

    #[test]
    fn a_timeout_is_indeterminate_never_absent() {
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false, value: None }, Err(Expired));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn an_html_front_end_is_indeterminate_not_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Html;
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn claimed_but_unprobeable_is_indeterminate_not_declared_only() {
        // Before `resolve_fetch` existed, an unparsed FetchWellKnown body
        // fell back to `declared.claimed` and could reach `DeclaredOnly`.
        // `resolve_fetch` judges the fetched body on its own terms and takes
        // no `claimed` flag at all, so an unparsed body is `Indeterminate`
        // regardless of what some other declaration claims elsewhere.
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::FetchWellKnown, None), Declared { claimed: true, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_filter_a_matching_boolean_from_a_500_is_indeterminate_not_verified() {
        // A capability claim needs the endpoint's own successful answer. A 500
        // body that happens to carry {"boolean": true} is not the engine
        // confirming the function is bound.
        let mut o = obs(Some(true));
        o.status = Some(500);
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_data_true_with_no_expectation_from_a_503_is_indeterminate() {
        let mut o = obs(Some(true));
        o.status = Some(503);
        let v = resolve(&def(ProbeKind::AskData, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn a_matching_boolean_from_a_2xx_is_still_a_capability_claim() {
        // The gate must not swallow the ordinary success case. This `def`
        // carries no `declared_by`, so the confirmation stands on its own.
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false, value: None }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn service_description_grading_separates_stub_from_substance() {
        // 21 of 28 descriptions in the wild are the same 14-triple Virtuoso
        // stub, so a stub must not score the same as a real description.
        assert_eq!(grade_service_description(14, false, false, false, false, false), Level(1));
        assert_eq!(grade_service_description(40, true, false, false, false, false), Level(2));
        assert_eq!(grade_service_description(7077, true, true, false, false, false), Level(3));
        assert_eq!(grade_service_description(7140, true, true, true, false, false), Level(4));
        assert_eq!(grade_service_description(0, false, false, false, false, false), Level(0));
    }

    #[test]
    fn extension_functions_and_example_resources_reach_level_four_like_entailment() {
        // Spec line 433: level 4 is "declares an entailment regime, example
        // resources, or extension functions". Only `has_entailment` was
        // checked, so a description declaring extension functions graded 1,
        // the same as a stub. That is precisely the rare honest declarer this
        // ladder exists to reward: 0 of 28 surveyed descriptions declared
        // anything geospatial, so when one finally does it must be credited.
        // The grade reads the document-wide flag, not the scoped set: a two
        // service document must grade the same from either endpoint.
        let fns = Declarations {
            triples: 20,
            doc_declares_extension_functions: true,
            ..Declarations::empty()
        };
        assert_eq!(grade_from_declarations(&fns), Level(4));

        // And the scoped set alone must NOT reach level 4, or the grade moves
        // with the probed endpoint and one document publishes two grades.
        let mut scoped_only = Declarations { triples: 20, ..Declarations::empty() };
        scoped_only
            .extension_functions
            .insert("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        assert_eq!(
            grade_from_declarations(&scoped_only),
            Level(1),
            "the scoped capability set must not drive the document's grade"
        );

        let examples = Declarations { triples: 20, has_example_resources: true, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&examples), Level(4));
    }

    #[test]
    fn a_stock_sd_feature_is_not_a_level_four_declaration() {
        // `sd:feature` is exactly what the 14-triple Virtuoso stub carries, so
        // widening level 4 must not sweep the stub up with it.
        let mut stub = Declarations { triples: 14, ..Declarations::empty() };
        stub.features.insert("http://www.w3.org/ns/sparql-service-description#UnionDefaultGraph".into());
        assert_eq!(grade_from_declarations(&stub), Level(1));
    }

    #[test]
    fn the_ladder_is_monotonic_in_every_declaration() {
        // A richer description must never grade lower than a poorer one, or
        // the number stops being readable as informativeness. Exhaustive over
        // all 32 combinations of the five declaration flags at a fixed
        // non-zero triple count: adding any one flag can only raise the grade.
        let grade = |bits: u8| {
            grade_service_description(
                20,
                bits & 1 != 0,
                bits & 2 != 0,
                bits & 4 != 0,
                bits & 8 != 0,
                bits & 16 != 0,
            )
            .0
        };
        for bits in 0u8..32 {
            for flag in [1u8, 2, 4, 8, 16] {
                if bits & flag != 0 {
                    continue;
                }
                assert!(
                    grade(bits | flag) >= grade(bits),
                    "adding flag {flag} to {bits} lowered the grade from {} to {}",
                    grade(bits),
                    grade(bits | flag)
                );
            }
        }
    }

    #[test]
    fn level_new_at_the_lower_boundary_is_some() {
        // The brief's tests cover the upper boundary (4, and the rejected 5)
        // but never the lower one.
        assert!(Level::new(0).is_some());
    }

    #[test]
    fn a_transport_error_is_indeterminate_not_absent() {
        // Only the `Html` half of the early guard was pinned before; this
        // covers the `error.is_some()` half.
        let mut o = obs(None);
        o.error = Some("connection reset".into());
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_data_false_with_no_expectation_is_absent() {
        let v = resolve(&def(ProbeKind::AskData, None), Declared { claimed: false, value: None }, Ok(&obs(Some(false))));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn cors_header_present_is_verified() {
        // cors: true by default in `obs`, whatever the status. No term in the
        // service-description vocabulary can declare CORS, so the metric
        // carries no `declared_by` and a confirmation is simply `Verified`.
        let v = resolve(&def(ProbeKind::Cors, None), Declared { claimed: false, value: None }, Ok(&obs(None)));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn cors_header_absent_with_a_successful_status_is_absent() {
        let mut o = obs(None);
        o.cors = false;
        let v = resolve(&def(ProbeKind::Cors, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn cors_header_absent_with_a_503_is_indeterminate() {
        // A proxy-generated error status cannot be attributed to the
        // endpoint's own CORS configuration.
        let mut o = obs(None);
        o.cors = false;
        o.status = Some(503);
        let v = resolve(&def(ProbeKind::Cors, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn allows_get_reads_a_method_list_entry_by_entry() {
        // An absent header is a grant: it is optional for a simple method, so a
        // preflight that answered without it refused nothing.
        assert!(allows_get(None), "an absent allow-methods header refuses nothing");
        assert!(allows_get(Some("*")));
        assert!(allows_get(Some("GET")));
        assert!(allows_get(Some("get")), "method names compare case-insensitively");
        assert!(allows_get(Some("GET, POST")));
        assert!(!allows_get(Some("POST")));
        // An empty header is NOT a grant: the server stated a list and GET is
        // not in it.
        assert!(!allows_get(Some("")), "an empty list grants nothing");
        // The case a naive `contains` gets wrong.
        assert!(!allows_get(Some("POSTGET")), "GET must be a list entry, not a substring");
    }

    #[test]
    fn grants_our_origin_accepts_only_a_wildcard_or_us() {
        assert!(!grants_our_origin(None), "no header is no grant");
        assert!(grants_our_origin(Some("*")));
        assert!(grants_our_origin(Some(ORIGIN)));
        assert!(
            grants_our_origin(Some(&ORIGIN.to_ascii_uppercase())),
            "an origin is host-insensitive to case, so an uppercased echo is still us"
        );
        // A grant to somebody else. Publishing `verified` off this would be a
        // confident wrong answer about an endpoint that would refuse us.
        assert!(!grants_our_origin(Some("https://example.com")));
        assert!(!grants_our_origin(Some("")));
    }

    /// The ordered status gate, at the unit level: the header check must not be
    /// reachable for a 405, whatever the headers say.
    #[test]
    fn a_preflight_405_is_absent_even_with_a_wildcard_grant() {
        let mut o = obs(None);
        o.body_kind = BodyKind::None;
        o.status = Some(405);
        o.allow_origin = Some("*".into());
        o.allow_methods = Some("GET, POST, OPTIONS".into());
        let v = resolve(&def(ProbeKind::CorsPreflight, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Absent, "the status gate must be consulted before any header");
    }

    /// A 3xx reaching the resolver is a chain `Client::preflight` could not
    /// resolve, and an unresolved chain is not an answer. It was `Absent`
    /// until C1: every other probe measures the endpoint through its redirect,
    /// so minting the redirect as an absence made one run publish `cors =
    /// undeclared-but-verified` and `cors-preflight = absent` about one
    /// service, from our own redirect policy rather than from the endpoint.
    /// Note that a wildcard grant rides along on each of these and changes
    /// nothing: the status gate still comes first.
    #[test]
    fn a_preflight_3xx_is_indeterminate_and_a_4xx_or_5xx_is_indeterminate() {
        for status in [301, 302, 303, 307, 308] {
            let mut o = obs(None);
            o.body_kind = BodyKind::None;
            o.status = Some(status);
            o.allow_origin = Some("*".into());
            let v = resolve(&def(ProbeKind::CorsPreflight, None), Declared { claimed: false, value: None }, Ok(&o));
            assert_eq!(
                v,
                Verdict::Indeterminate,
                "an unresolved redirect chain never reached a preflight answer: {status}"
            );
        }
        for status in [400, 401, 403, 404, 429, 500, 502, 503] {
            let mut o = obs(None);
            o.body_kind = BodyKind::None;
            o.status = Some(status);
            o.allow_origin = Some("*".into());
            let v = resolve(&def(ProbeKind::CorsPreflight, None), Declared { claimed: false, value: None }, Ok(&o));
            assert_eq!(v, Verdict::Indeterminate, "status {status} describes our request, not a CORS policy");
        }
    }

    #[test]
    fn an_expired_or_failed_preflight_is_indeterminate_never_absent() {
        let d = def(ProbeKind::CorsPreflight, None);
        assert_eq!(resolve(&d, Declared { claimed: false, value: None }, Err(Expired)), Verdict::Indeterminate);
        let mut o = obs(None);
        o.body_kind = BodyKind::None;
        o.status = None;
        o.error = Some("connection reset".into());
        assert_eq!(resolve(&d, Declared { claimed: false, value: None }, Ok(&o)), Verdict::Indeterminate);
    }

    #[test]
    fn liveness_sparql_json_is_verified() {
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false, value: None }, Ok(&obs(None)));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn liveness_other_body_is_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn select_iris_with_bindings_is_verified() {
        let mut o = obs(None);
        o.bindings = vec!["http://example.org/x".into()];
        let v = resolve(&def(ProbeKind::SelectIris, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn select_iris_bindings_from_a_500_is_indeterminate_not_verified() {
        // M1. `AskData`'s positive case has been gated on `answered_ok` since
        // the same argument was made about it: a 500 body that happens to
        // carry a populated `results.bindings` is not the engine confirming
        // that the endpoint holds classes. Two metrics reading the same
        // evidence shape must not apply different rules to it.
        let mut o = obs(None);
        o.status = Some(500);
        o.bindings = vec!["http://example.org/C".into()];
        let v = resolve(&def(ProbeKind::SelectIris, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    /// One rule for what a confirmation publishes, in every arm that can
    /// confirm one. A metric no declaration could speak for is `Verified` on
    /// the probe alone; a metric that names a `declared_by` is
    /// `UndeclaredButVerified` until the description actually says so. An arm
    /// that hardcodes either verdict fails here.
    #[test]
    fn every_confirming_arm_reads_the_declaration_axis_the_same_way() {
        // (kind, expect, the observation that confirms it)
        let cases: Vec<(ProbeKind, Option<bool>, Observation)> = vec![
            (ProbeKind::AskFilter, Some(true), obs(Some(true))),
            (ProbeKind::AskData, None, obs(Some(true))),
            (ProbeKind::Cors, None, obs(None)),
            (ProbeKind::CorsPreflight, None, {
                let mut o = obs(None);
                o.body_kind = BodyKind::None;
                o.allow_origin = Some("*".into());
                o
            }),
            (ProbeKind::Liveness, None, obs(None)),
            (ProbeKind::SelectIris, None, {
                let mut o = obs(None);
                o.bindings = vec!["http://example.org/C".into()];
                o
            }),
        ];
        for (kind, expect, o) in cases {
            // Nothing in the vocabulary could declare this one.
            let undeclarable = def(kind, expect);
            assert_eq!(
                resolve(&undeclarable, Declared { claimed: false, value: None }, Ok(&o)),
                Verdict::Verified,
                "{kind:?}: a confirmation of a capability nothing could declare is Verified"
            );

            let mut declarable = def(kind, expect);
            declarable.declared_by = Some(SF_WITHIN.into());
            assert_eq!(
                resolve(&declarable, Declared { claimed: false, value: None }, Ok(&o)),
                Verdict::UndeclaredButVerified,
                "{kind:?}: a declarable capability confirmed but not declared is undeclared-but-verified"
            );
            assert_eq!(
                resolve(&declarable, Declared { claimed: true, value: None }, Ok(&o)),
                Verdict::Verified,
                "{kind:?}: declared and confirmed is Verified"
            );
        }
    }

    #[test]
    fn select_iris_empty_bindings_from_a_parsed_result_is_absent() {
        // body_kind SparqlJson and status 200 by default in `obs`.
        let v = resolve(&def(ProbeKind::SelectIris, None), Declared { claimed: false, value: None }, Ok(&obs(None)));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn select_iris_empty_bindings_from_an_unparsed_body_is_indeterminate() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::SelectIris, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn fetch_well_known_unparsed_body_unclaimed_is_indeterminate_not_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::FetchWellKnown, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn liveness_a_429_is_indeterminate_not_absent() {
        // A throttled endpoint is emphatically alive. Reporting availability
        // as `Absent` here would tombstone it as unreachable, on the one
        // metric a later stage reads to decide whether to admit it at all.
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        o.status = Some(429);
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn liveness_a_503_from_an_intermediary_is_indeterminate_not_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        o.status = Some(503);
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_data_false_with_a_non_2xx_status_is_indeterminate_not_absent() {
        // An error body that happens to parse as SPARQL JSON with no bindings
        // is not evidence that the data is missing.
        let mut o = obs(Some(false));
        o.status = Some(500);
        let v = resolve(&def(ProbeKind::AskData, None), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_filter_wrong_boolean_with_a_non_2xx_status_is_indeterminate() {
        // `DeclaredButWrong` is ranked worse than absent, so minting it from a
        // response the endpoint never authored is the worst available error.
        let mut o = obs(Some(false));
        o.status = Some(502);
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false, value: None }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn a_declaration_the_endpoint_actually_makes_yields_verified() {
        let mut d = Declarations::empty();
        d.features.insert("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let mut def = def(ProbeKind::AskFilter, Some(true));
        def.declared_by = Some("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let v = resolve(&def, Declared::from(&d, &def), Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::Verified, "declared and working is Verified");
    }

    #[test]
    fn working_but_undeclared_stays_undeclared_even_with_declarations_wired() {
        // This is the expected outcome for almost every real endpoint: 18
        // evaluate geof:sfWithin and none of them declares it. Wiring
        // declarations must NOT change these to Verified.
        let d = Declarations::empty();
        let mut def = def(ProbeKind::AskFilter, Some(true));
        def.declared_by = Some("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let v = resolve(&def, Declared::from(&d, &def), Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn a_metric_no_declaration_can_speak_for_is_never_claimed() {
        let mut d = Declarations::empty();
        d.features.insert("http://example.org/anything".into());
        let def = def(ProbeKind::Cors, None); // declared_by is None
        assert!(!Declared::from(&d, &def).claimed);
    }

    #[test]
    fn a_fetched_stub_grades_low_and_a_substantial_one_grades_high() {
        let stub = Declarations { triples: 14, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&stub), Level(1));
        let rich = Declarations { triples: 7077, names_dataset: true, has_void_partitions: true, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&rich), Level(3));
    }

    #[test]
    fn a_fetched_rdf_description_is_verified_with_a_level() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Rdf;
        let d = Declarations { triples: 14, ..Declarations::empty() };
        let (v, level) = resolve_fetch(&d, Ok(&o));
        assert_eq!(v, Verdict::Verified);
        assert_eq!(level, Some(Level(1)));
    }

    #[test]
    fn an_rdf_body_from_a_429_is_indeterminate_not_verified() {
        // The arm that returns `Verified` on a parsed RDF body used to run
        // before the status match, so any status with an RDF-ish payload
        // published a confident `verified` plus a graded level. A throttle
        // notice is the case where we know least about what is published.
        let mut o = obs(None);
        o.status = Some(429);
        o.body_kind = BodyKind::Rdf;
        let d = Declarations { triples: 14, ..Declarations::empty() };
        let (v, level) = resolve_fetch(&d, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None, "an ungated grade is a confident wrong answer");
    }

    #[test]
    fn an_rdf_body_from_a_500_or_503_is_indeterminate_not_verified() {
        for status in [500, 502, 503] {
            let mut o = obs(None);
            o.status = Some(status);
            o.body_kind = BodyKind::Rdf;
            let d = Declarations { triples: 40, names_dataset: true, ..Declarations::empty() };
            let (v, level) = resolve_fetch(&d, Ok(&o));
            assert_eq!(v, Verdict::Indeterminate, "status {status}");
            assert_eq!(level, None, "status {status}");
        }
    }

    #[test]
    fn an_rdf_body_from_a_404_is_absent_with_level_zero_not_verified() {
        // A non-2xx with an RDF body must fall through to the status match,
        // where 404 keeps its genuine absence rather than being overtaken by
        // the body classification.
        let mut o = obs(None);
        o.status = Some(404);
        o.body_kind = BodyKind::Rdf;
        let d = Declarations { triples: 14, ..Declarations::empty() };
        let (v, level) = resolve_fetch(&d, Ok(&o));
        assert_eq!(v, Verdict::Absent);
        assert_eq!(level, Some(Level(0)));
    }

    #[test]
    fn a_404_on_the_description_is_absent_not_indeterminate() {
        // A 404 is a real answer: the endpoint served us, and there is no
        // description there. That is one of the few honest absences.
        let mut o = obs(None);
        o.status = Some(404);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Absent);
        assert_eq!(level, Some(Level(0)));
    }

    #[test]
    fn a_410_on_the_description_is_absent_not_indeterminate() {
        // Gone is the other genuine absence: it was published and has since
        // been withdrawn.
        let mut o = obs(None);
        o.status = Some(410);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Absent);
        assert_eq!(level, Some(Level(0)));
    }

    #[test]
    fn a_400_from_a_queryless_get_rejection_is_indeterminate_not_absent() {
        // 10 of 548 surveyed endpoints return a plain 400 to exactly the
        // queryless GET this probe makes: the server rejected the shape of
        // the request, which says nothing about whether a description is
        // published there. Treating 400 as absence would falsely tombstone
        // every one of them.
        let mut o = obs(None);
        o.status = Some(400);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None);
    }

    #[test]
    fn a_429_throttle_is_indeterminate_not_absent() {
        // Being throttled is the one situation where we know least about
        // what exists; it must not resolve to the same verdict as knowing
        // for certain that nothing is published.
        let mut o = obs(None);
        o.status = Some(429);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None);
    }

    #[test]
    fn a_401_or_403_on_the_description_is_indeterminate_not_absent() {
        // Refused is not the same as nothing being there: a real, well-formed
        // description can sit behind auth.
        for status in [401, 403] {
            let mut o = obs(None);
            o.status = Some(status);
            o.body_kind = BodyKind::Other;
            let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
            assert_eq!(v, Verdict::Indeterminate, "status {status}");
            assert_eq!(level, None, "status {status}");
        }
    }

    #[test]
    fn an_unparseable_description_body_is_indeterminate() {
        let mut o = obs(None);
        o.status = Some(200);
        o.body_kind = BodyKind::Other;
        let (v, _) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn an_expired_fetch_is_indeterminate_with_no_level() {
        let (v, level) = resolve_fetch(&Declarations::empty(), Err(Expired));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None);
    }
}
