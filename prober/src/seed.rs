//! Turning a LOD Cloud dump into a candidate registry.
//!
//! The dump is a JSON object of datasets, each carrying a `sparql` array whose
//! entries have an `access_url`. Measured on the dump whose own version is
//! 2026-06-15, fetched on 2026-08-19: 1683 datasets, all of them carrying the
//! key, 970 of those arrays empty, 725 entries, 548 distinct URLs. The version
//! and the fetch date are different things, which is why `seed-registry` takes
//! them as separate flags.
//!
//! Two things this module deliberately does not do.
//!
//! It does not consult the dump's own `status` field to admit or refuse a
//! candidate. That field reports 72 URLs OK where the survey behind this
//! project's metric set found 65 answering a query, so it is a third party's
//! stale judgement, and admitting or refusing on it would be this project
//! publishing somebody else's confidence as its own. It is recorded as
//! provenance and it gates nothing.
//!
//! It does not regenerate `registry/exclusions.toml`, and it never writes it.
//! It reads the path `--exclusions` names, at every run.
//! That file is the only thing here that survives a re-seed: a host deleted
//! from the seeded registry by hand is back in it at the next seed, and an
//! entry in the exclusion list is not. The list is applied, so an excluded host
//! is absent from a freshly seeded registry rather than merely skipped by a
//! sweep. See `registry::without_excluded`.
//!
//! It does not decide whether an endpoint WORKS. That question needs a probe
//! and is a later slice. Every refusal here is decidable from the string with
//! nobody contacted, which is what makes it honest to apply before a sweep
//! rather than after one.

use crate::registry;

/// What one dump yielded: the candidates to sweep, and the counts that make the
/// result checkable against a re-run.
///
/// The counts exist because a seeded registry is a derived artefact whose only
/// provenance is arithmetic. Without them, "re-seed and see whether it matches"
/// is not a question anyone can answer.
#[derive(Debug, PartialEq, Eq)]
pub struct Seeded {
    /// The candidates, in dataset-key order, after every refusal.
    pub endpoints: Vec<String>,
    pub counts: Counts,
}

/// One count per stage of the pipeline, so a difference between two seeds can
/// be attributed to a stage rather than guessed at.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Datasets in the dump, whether or not they name an endpoint.
    pub datasets: usize,
    /// Datasets carrying at least one non-empty `access_url`.
    pub datasets_with_entries: usize,
    /// `access_url` values found, before deduplication.
    pub entries: usize,
    /// Distinct values after `registry::dedupe`.
    pub distinct: usize,
    /// Dropped because the authority carried userinfo.
    pub refused_credentials: usize,
    /// Dropped because somebody asked this project not to probe the host.
    pub refused_excluded: usize,
    /// Dropped because the host names the local machine or a private network,
    /// which an operator may target but a third-party dump may not nominate.
    pub refused_unroutable: usize,
    /// Dropped because the host is reserved for documentation or guaranteed not
    /// to resolve, so no service can be there to measure.
    pub refused_reserved: usize,
    /// Dropped because the value cannot be an IRI, so every fact about it would
    /// be unpublishable.
    pub refused_unpublishable: usize,
}

impl Counts {
    /// The candidates left, which must equal `endpoints.len()`.
    ///
    /// Stated as arithmetic rather than read off the vector so that the two can
    /// be compared: a mismatch means a refusal was applied without being
    /// counted, and the provenance file would then assert a total nothing
    /// produced.
    pub fn seeded(&self) -> usize {
        self.distinct
            - self.refused_credentials
            - self.refused_excluded
            - self.refused_unroutable
            - self.refused_reserved
            - self.refused_unpublishable
    }
}

/// The candidates a dump yields, with every refusal applied and counted.
///
/// Order is ascending by dataset key, then by position within that dataset's
/// `sparql` array. That is not document order: `serde_json` without its
/// `preserve_order` feature backs an object with a `BTreeMap`, so iteration is
/// sorted by key, and enabling the feature to get document order would pull in
/// `indexmap` to buy a property nothing here needs.
///
/// What the order has to be is **deterministic and stable**, because
/// `run_sweep` dispatches host groups in first-seen order, so the file's order
/// decides which endpoints a bounded run reaches first, and because two seeds
/// of the same dump must diff cleanly. Sorted-by-key delivers both, and it is
/// arguably the better of the two: it does not move if the dump's serialiser
/// changes how it lays keys out.
///
/// The refusals run in a fixed order, and it matters only for the counts: a URL
/// that is both credentialed and unpublishable is counted under whichever comes
/// first. Credentials come first so that nothing after them can log a password,
/// which is the same reason `registry::without_unpublishable_iris` documents for
/// its own position.
/// The exclusion list comes second, for the same reason `load_endpoints` puts it
/// early: a host somebody asked us to leave alone is counted as excluded rather
/// than as an unroutable or unpublishable one, so `refused_excluded` in the
/// provenance file is the number of candidates a request removed. It is also the
/// only refusal here that can change between two seeds of the SAME dump, which
/// is what makes counting it separately the difference between an attributable
/// diff and a guess.
pub fn candidates(dump: &[u8], excluded: &[registry::Exclusion]) -> anyhow::Result<Seeded> {
    let parsed: serde_json::Value = serde_json::from_slice(dump)
        .map_err(|e| anyhow::anyhow!("the dump is not valid JSON: {e}"))?;
    let datasets = parsed.as_object().ok_or_else(|| {
        anyhow::anyhow!("the dump's top level is not an object of datasets, so it names none")
    })?;

    let mut counts = Counts { datasets: datasets.len(), ..Counts::default() };
    let mut found: Vec<String> = Vec::new();
    for dataset in datasets.values() {
        let before = found.len();
        // A missing `sparql` key and an empty array mean the same thing here:
        // this dataset names no endpoint. 970 of 1683 carry an empty array and
        // none lacks the key, but reading both the same way costs nothing and
        // survives a dump that changes its mind.
        for entry in dataset.get("sparql").and_then(|s| s.as_array()).into_iter().flatten() {
            if let Some(url) = entry.get("access_url").and_then(|u| u.as_str()) {
                // Not trimmed: `dqv:computedOn` publishes the registry string
                // verbatim, so a trimmed spelling is a string the registry does
                // not contain. A value carrying whitespace is refused later, by
                // the IRI rule, under its own reason.
                if !url.is_empty() {
                    found.push(url.to_string());
                }
            }
        }
        if found.len() > before {
            counts.datasets_with_entries += 1;
        }
    }
    counts.entries = found.len();

    let distinct = registry::dedupe(&found);
    counts.distinct = distinct.len();

    let named = registry::without_credentials(&distinct);
    counts.refused_credentials = distinct.len() - named.len();

    // Applied here as well as in `load_endpoints`, and not because a sweep
    // could otherwise reach the host: because this list is written to a file
    // that is committed and public, and a re-seed regenerates it, so a host
    // deleted by hand comes back. See `registry::without_excluded`. An error
    // stops the seed: a registry written without the exclusion list applied
    // would be indistinguishable afterwards from one written with it.
    let wanted = registry::without_excluded(&named, excluded);
    counts.refused_excluded = named.len() - wanted.len();

    // Seeder-only, and the one refusal that is not in `load_endpoints`. See
    // `registry::without_unroutable_hosts` for why the two live apart.
    let routable = registry::without_unroutable_hosts(&wanted);
    counts.refused_unroutable = wanted.len() - routable.len();

    let unreserved = registry::without_reserved_names(&routable);
    counts.refused_reserved = routable.len() - unreserved.len();

    let publishable = registry::without_unpublishable_iris(&unreserved);
    counts.refused_unpublishable = unreserved.len() - publishable.len();

    debug_assert_eq!(counts.seeded(), publishable.len(), "a refusal went uncounted");
    Ok(Seeded { endpoints: publishable, counts })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/lod-cloud-sample.json");

    /// The exact list, not a count: a count passes on an extractor that reads
    /// the wrong field from the right number of entries.
    ///
    /// Every one of these came out of the real dump, and the fixture records
    /// which shape each dataset was chosen for.
    #[test]
    fn the_sample_dump_yields_the_candidates_it_names() {
        let seeded = candidates(SAMPLE, &[]).unwrap();
        assert_eq!(
            seeded.endpoints,
            vec![
                // Dataset-key order: cz-ctia-bans, dbpedia-ja, then foodista's
                // two entries in array order. Asserting the order and not just
                // the membership is what pins the stability `run_sweep`'s
                // dispatch and a seed-to-seed diff both rely on.
                "http://linked.opendata.cz/sparql".to_string(),
                "http://ja.dbpedia.org/sparql".to_string(),
                "http://kasabi.com/api/sparql-endpoint-foodista".to_string(),
                "http://api.kasabi.com/dataset/foodista/apis/sparql".to_string(),
            ]
        );
    }

    /// 970 of the dump's 1683 datasets carry an EMPTY `sparql` array. None
    /// lacks the key, so a fixture built around a missing key would test a
    /// shape the dump never contains.
    #[test]
    fn a_dataset_whose_sparql_array_is_empty_contributes_nothing() {
        let seeded = candidates(SAMPLE, &[]).unwrap();
        assert_eq!(seeded.counts.datasets, 9);
        assert_eq!(
            seeded.counts.datasets_with_entries, 8,
            "one of the nine names no endpoint"
        );
    }

    /// An `access_url` of the empty string names no endpoint, so it is not an
    /// entry.
    ///
    /// The fixture's third foodista entry is HAND-ADDED: 0 of the real dump's
    /// 725 entries have an empty or missing `access_url`, so this guard has no
    /// input in the dump that reaches it and, without this fixture entry,
    /// replacing `if !url.is_empty()` with `if true` changes nothing anywhere.
    /// With it, the empty string would be counted as an entry, survive
    /// deduplication, and then be refused by the IRI rule under a reason that
    /// says the registry contained a URL, which is a fact about nothing.
    #[test]
    fn an_empty_access_url_is_not_an_entry() {
        let seeded = candidates(SAMPLE, &[]).unwrap();
        assert_eq!(seeded.counts.entries, 9, "the empty value is not a tenth entry");
        assert_eq!(
            seeded.counts.refused_unpublishable, 1,
            "only the {{SPARQL}} placeholder, so the empty string never reached the IRI rule"
        );
        assert!(
            !seeded.endpoints.iter().any(|e| e.is_empty()),
            "an empty candidate cannot be probed or published: {:?}",
            seeded.endpoints
        );
    }

    /// 725 entries collapse to 548 in the real dump, so this is the common case
    /// rather than an edge.
    #[test]
    fn two_datasets_naming_one_endpoint_yield_one_candidate() {
        let seeded = candidates(SAMPLE, &[]).unwrap();
        assert_eq!(seeded.counts.entries, 9);
        assert_eq!(seeded.counts.distinct, 8);
        assert_eq!(
            seeded.endpoints.iter().filter(|e| *e == "http://linked.opendata.cz/sparql").count(),
            1
        );
    }

    /// The dump's `status` field is a third party's stale judgement: it reports
    /// 72 URLs OK where the survey found 65 answering. A `FAIL` entry is still
    /// a candidate, and `dbpedia-ja` in the fixture is one.
    #[test]
    fn the_dumps_own_status_field_does_not_gate_a_candidate() {
        let seeded = candidates(SAMPLE, &[]).unwrap();
        let failed = "http://ja.dbpedia.org/sparql";
        assert!(
            seeded.endpoints.iter().any(|e| e == failed),
            "a FAIL status must not refuse a candidate: {:?}",
            seeded.endpoints
        );
    }

    /// Each refusal is counted under its own reason, so a provenance file can
    /// say which rule dropped what. Counting them as one bucket would make a
    /// difference between two seeds unattributable.
    #[test]
    fn every_refusal_is_counted_under_its_own_reason() {
        let counts = candidates(SAMPLE, &[]).unwrap().counts;
        assert_eq!(counts.refused_credentials, 1, "the hand-added one");
        assert_eq!(
            counts.refused_excluded, 0,
            "the sample dump names no host on the exclusion list"
        );
        assert_eq!(counts.refused_unroutable, 1, "localhost:3030, from the real dump");
        assert_eq!(counts.refused_reserved, 1, "example.org, from the real dump");
        assert_eq!(counts.refused_unpublishable, 1, "the {{SPARQL}} placeholder");
    }

    /// The arithmetic and the vector must agree, or a refusal was applied
    /// without being counted and the provenance would assert a total nothing
    /// produced.
    #[test]
    fn the_counts_add_up_to_the_list() {
        let seeded = candidates(SAMPLE, &[]).unwrap();
        assert_eq!(seeded.counts.seeded(), seeded.endpoints.len());
        assert_eq!(seeded.endpoints.len(), 4);
    }

    #[test]
    fn malformed_json_is_an_error_naming_the_problem() {
        let error = candidates(b"{not json", &[]).unwrap_err().to_string();
        assert!(error.contains("not valid JSON"), "unhelpful: {error}");
    }

    #[test]
    fn a_dump_that_is_not_an_object_of_datasets_is_an_error() {
        let error = candidates(b"[]", &[]).unwrap_err().to_string();
        assert!(error.contains("names none"), "unhelpful: {error}");
    }

    /// An excluded host is absent from a FRESHLY SEEDED registry, not merely
    /// absent from a sweep.
    ///
    /// This is the half `load_endpoints` cannot deliver. Deleting a URL from
    /// `registry/lod-cloud.toml` by hand is undone by the next re-seed, so
    /// without this the committed artefact would keep naming a host that asked
    /// to be left alone, and anyone reading the file would have no way to know
    /// the sweep skips it.
    ///
    /// An inline dump rather than the shared fixture: the fixture's counts are
    /// asserted by five other tests here, and this one needs a dataset the
    /// exclusion list actually names. Dataset keys are in ascending order, so
    /// `asked` precedes `kept`.
    #[test]
    fn an_excluded_host_is_absent_from_a_fresh_seed_and_counted_as_excluded() {
        let dump = br#"{
          "asked": {"sparql": [{"access_url": "https://sparqlwatch-exclusion-worked-example/sparql"}]},
          "kept": {"sparql": [{"access_url": "https://kept.test-host/sparql"}]}
        }"#;
        let excluded = registry::parse_exclusions(
            "[[exclusion]]\nhost = \"sparqlwatch-exclusion-worked-example\"\nreason = \"a \
             person asked, 2026-08-25\"\n",
        )
        .unwrap();
        let seeded = candidates(dump, &excluded).unwrap();
        assert_eq!(
            seeded.endpoints,
            vec!["https://kept.test-host/sparql".to_string()],
            "an excluded host may not be written into the registry"
        );
        assert_eq!(seeded.counts.distinct, 2, "both were candidates before the refusals");
        assert_eq!(
            seeded.counts.refused_excluded, 1,
            "counted under its own reason, because an exclusion is the one refusal that can \
             change between two seeds of the SAME dump"
        );
        assert_eq!(seeded.counts.seeded(), seeded.endpoints.len());
    }
}
