//! Write a candidate registry, and the provenance that says which dump it came
//! from, out of a LOD Cloud dump.
//!
//! `seed::candidates` does the extraction and every refusal. This binary is the
//! thin shell around it: read the dump, check it is the dump the caller named,
//! render two files.
//!
//! What it writes is NOT `prober/endpoints.toml`. That file is the
//! three-endpoint development list, `registry.rs` asserts it loads exactly
//! three entries with `qlever.dev` first, and it is the default `--endpoints`,
//! so a plain `cargo run` uses it. None of its three endpoints appears in the
//! dump. The two files are different lists for different purposes, and this
//! binary has no flag that can reach the other one.
//!
//! There is no `--limit`. A bounded sample is cut by whoever needs one, from
//! the file this writes, and a flag here would put the bound in the seeded
//! artefact instead of in the run that wanted it.
//!
//! Nothing here records a wall clock, so re-seeding the same dump writes the
//! same two files byte for byte and a diff shows only what the dump or the rule
//! changed.

use clap::Parser;
use serde::{Deserialize, Serialize};
use sparqlwatch_prober::seed;

#[derive(Parser)]
#[command(name = "seed-registry")]
struct Args {
    /// The LOD Cloud dump to read. It is not committed: it is 4 MB of a third
    /// party's data, and `[dump]` in the provenance file says where to get it.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    dump: String,
    /// The SHA-256 the dump is expected to have, lowercase hex. Compared
    /// against the digest of the bytes actually read, and nothing is written if
    /// the two differ, so the hash in the provenance file describes the input
    /// that produced the list beside it rather than what somebody typed.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    sha256: String,
    /// Where the dump was fetched from. Give the VERSIONED URL: the hash is
    /// only useful if the exact bytes can be fetched again and checked, and an
    /// unversioned path is the case where the hash records what was used and
    /// nothing recovers it.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    source: String,
    /// The dump's own version, as the source names it.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    dump_version: String,
    /// The date the dump was fetched. Distinct from its version: a versioned
    /// dump can be fetched long after it was published.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    downloaded: String,
    /// The exclusion list to apply: the hosts somebody asked this project not
    /// to probe. Read at run time and never written, so a re-seed leaves it as
    /// it is. Nothing is written if it cannot be read.
    #[arg(long, default_value = sparqlwatch_prober::registry::DEFAULT_EXCLUSIONS)]
    exclusions: String,
    #[arg(long, default_value = "registry/lod-cloud.toml")]
    out: String,
    #[arg(long, default_value = "registry/lod-cloud.provenance.toml")]
    provenance: String,
}

/// Which dump produced a registry file, and every count from the extraction.
///
/// A parseable file rather than a comment header, because a comment cannot be
/// read back: the test below loads this and the registry beside it and compares
/// the two, and a provenance file that disagrees with its list is worse than
/// none.
#[derive(Serialize, Deserialize)]
struct Provenance {
    dump: Dump,
    extraction: Extraction,
    counts: CountsRecord,
}

/// The input, identified well enough to fetch it again and check it.
#[derive(Serialize, Deserialize)]
struct Dump {
    source: String,
    version: String,
    downloaded: String,
    bytes: usize,
    sha256: String,
}

/// Which code did the extracting.
///
/// The crate version is coarse: it does not move on every change to
/// `seed::candidates`, so a re-seed of the same `sha256` that yields different
/// counts at the same `crate_version` means the rule changed without the
/// version being raised. It is recorded anyway because it is the only version
/// this crate states about itself, and naming the function makes the rule
/// findable from the artefact.
#[derive(Serialize, Deserialize)]
struct Extraction {
    rule: String,
    crate_version: String,
}

/// `seed::Counts`, flattened for TOML, with the total written out.
///
/// `seeded` is stated rather than left implied so this file can be checked
/// against the list beside it without re-running anything, and against its own
/// arithmetic: `distinct` minus the five refusals.
#[derive(Serialize, Deserialize)]
struct CountsRecord {
    datasets: usize,
    datasets_with_entries: usize,
    entries: usize,
    distinct: usize,
    refused_credentials: usize,
    refused_excluded: usize,
    refused_unroutable: usize,
    refused_reserved: usize,
    refused_unpublishable: usize,
    seeded: usize,
}

/// What a generated file says about itself, so a reader who opens one knows it
/// is derived and knows what to read instead of editing it.
///
/// Two headers rather than one shared line, because the registry file points at
/// the provenance file and the provenance file cannot point at itself.
const GENERATED: &str = "\
# Generated by `cargo run --bin seed-registry`. Hand edits are overwritten by
# the next re-seed, and the fixed-point test in src/bin/seed-registry.rs fails
# on them.
";
const REGISTRY_HEADER: &str = "\
# Which dump this came from, and every count behind it, are in
# lod-cloud.provenance.toml beside this file. This is NOT the development list:
# that is prober/endpoints.toml, and it is the default --endpoints.
";
const PROVENANCE_HEADER: &str = "\
# Which dump produced lod-cloud.toml beside this file, and every count from the
# extraction. `seeded` is that file's length.
";

/// The registry file's text: an `endpoint` array, in the order given, in the
/// shape `registry::load_endpoints` reads.
///
/// The array is serialised by the `toml` crate rather than assembled from
/// `format!`, so quoting and escaping are the same code that parses it back.
/// That matters for a file of hundreds of URLs, some of them carrying a query
/// string or a percent escape, where a hand-rolled quote would be one `&` or `%`
/// away from a value the loader reads differently from the one seeded.
/// `to_string_pretty` is what puts one URL per line, which is what makes two
/// seeds diff readably.
fn registry_toml(endpoints: &[String]) -> String {
    #[derive(Serialize)]
    struct EndpointFile<'a> {
        endpoint: &'a [String],
    }
    // Infallible in practice: the value is one array of strings, and `toml`
    // fails on shapes TOML cannot hold, such as a map keyed by something other
    // than a string. Reported rather than unwrapped so a future field cannot
    // turn a serialisation problem into a panic in a generator.
    let body = toml::to_string_pretty(&EndpointFile { endpoint: endpoints })
        .expect("an array of strings is representable in TOML");
    format!("{GENERATED}{REGISTRY_HEADER}{body}")
}

/// The provenance file's text.
fn provenance_toml(provenance: &Provenance) -> String {
    let body = toml::to_string_pretty(provenance)
        .expect("the provenance record is strings and integers in three tables");
    format!("{GENERATED}{PROVENANCE_HEADER}{body}")
}

/// The lowercase hex SHA-256 of `bytes`, per FIPS 180-4.
///
/// Computed here rather than taken on trust from `--sha256` because the hash is
/// the whole provenance of a derived list: it says WHICH dump produced it, so a
/// re-seed that differs can be attributed to a new dump rather than to a changed
/// rule. A hash copied from a flag into the file is one typo away from naming a
/// dump that never produced anything, and nothing downstream could notice.
///
/// Hand-written because this crate has no digest dependency and a checksum for
/// provenance does not warrant adding one. The published vectors are asserted in
/// the tests below, including the 56-byte case where the length pads into a
/// second block.
fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // The padded message: the bytes, a single 1 bit, zeros, and the original
    // length in bits as a big-endian u64, sized so the whole thing is a whole
    // number of 64-byte blocks. The loop rather than one modulo is what handles
    // an input whose remainder leaves no room for the length: it then pads out
    // this block and the length lands in the next one.
    let mut padded = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // as_chunks rather than chunks_exact, for both the 64-byte blocks and the
    // 4-byte words. The padding above guarantees a whole number of blocks, so
    // `.0` is the whole of `padded` and the discarded remainder is empty. What
    // this buys beyond clippy's approval is the `try_into` below: as_chunks
    // yields arrays rather than slices, so the length is known to the type
    // system and the runtime check that could not fail is gone with it.
    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (word, raw) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*raw);
        }
        for i in 16..64 {
            let a = w[i - 15];
            let b = w[i - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = state;
        for (k, w) in K.iter().zip(w.iter()) {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let choose = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(*k)
                .wrapping_add(*w);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let majority = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(majority);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (h, v) in state.iter_mut().zip(v.iter()) {
            *h = h.wrapping_add(*v);
        }
    }

    state.iter().map(|word| format!("{word:08x}")).collect()
}

/// Fail unless the dump's own digest is the one the caller named.
///
/// Before anything is extracted: a list seeded from a dump other than the one
/// the provenance file will name is a list whose provenance is wrong, and no
/// later check could tell.
///
/// A function of its own, and not three lines inside `main`, because it is one
/// of the two gates the whole tool exists to hold and `main` had no test. A
/// review of this branch disabled the comparison and all 351 tests stayed
/// green.
///
/// The claim is lowercased so `--sha256` may be pasted from a tool that prints
/// uppercase hex. The digest is not: `sha256_hex` writes `{:08x}`.
fn check_digest(dump_path: &str, digest: &str, claimed: &str) -> anyhow::Result<()> {
    let claimed = claimed.to_ascii_lowercase();
    if digest != claimed {
        anyhow::bail!(
            "{dump_path} is not the dump named on the command line: it hashes to {digest}, not \
             to {claimed}. Nothing was written, because a registry seeded from one dump and a \
             provenance file naming another cannot be told apart afterwards.",
        );
    }
    Ok(())
}

/// Fail unless the counts add up to the list they will be written beside.
///
/// `seed::candidates` states this as a `debug_assert`, which is compiled out of
/// a release build. Checked again because this is where the number becomes a
/// permanent claim in a file, and the two files would then disagree about a
/// total neither produced. No input can reach the bail today, which is why it
/// is a function with a test rather than a line inside `main`: replacing it with
/// `if false` is invisible to a suite that can only drive it through
/// `seed::candidates`.
///
/// What no test here can show is that `main` still CALLS it. Removing the call
/// was tried and survived the whole suite, because there is no dump for which
/// the arithmetic disagrees, so nothing observable changes. Recorded rather than
/// papered over: the function's own behaviour is pinned by
/// `counts_that_do_not_add_up_to_the_list_are_refused`, and the `debug_assert`
/// in `seed::candidates` is the second place the same invariant is stated.
fn check_counts(seeded: &seed::Seeded) -> anyhow::Result<()> {
    if seeded.counts.seeded() != seeded.endpoints.len() {
        anyhow::bail!(
            "the counts do not add up: {} seeded by arithmetic against {} candidates kept, so a \
             refusal was applied without being counted",
            seeded.counts.seeded(),
            seeded.endpoints.len(),
        );
    }
    Ok(())
}

/// The provenance record for one seeding run.
///
/// `bytes` and `sha256` come from the dump that was read, not from the flags,
/// for the same reason the digest is computed rather than trusted: the file has
/// to describe the input that produced the list beside it. The three strings
/// that CANNOT be derived from the bytes, `source`, `version` and `downloaded`,
/// are the caller's word and are the only fields taken from flags.
fn provenance_of(args: &Args, dump: &[u8], digest: &str, counts: &seed::Counts) -> Provenance {
    Provenance {
        dump: Dump {
            source: args.source.clone(),
            version: args.dump_version.clone(),
            downloaded: args.downloaded.clone(),
            bytes: dump.len(),
            sha256: digest.to_string(),
        },
        extraction: Extraction {
            rule: "sparqlwatch_prober::seed::candidates".to_string(),
            crate_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        counts: CountsRecord {
            datasets: counts.datasets,
            datasets_with_entries: counts.datasets_with_entries,
            entries: counts.entries,
            distinct: counts.distinct,
            refused_credentials: counts.refused_credentials,
            refused_excluded: counts.refused_excluded,
            refused_unroutable: counts.refused_unroutable,
            refused_reserved: counts.refused_reserved,
            refused_unpublishable: counts.refused_unpublishable,
            seeded: counts.seeded(),
        },
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let dump = std::fs::read(&args.dump)?;

    let digest = sha256_hex(&dump);
    check_digest(&args.dump, &digest, &args.sha256)?;

    // Read before anything is written, and an error here stops the run: a
    // registry written without the exclusion list applied cannot be told
    // afterwards from one written with it, and it is committed and public.
    let excluded =
        sparqlwatch_prober::registry::read_exclusions(std::path::Path::new(&args.exclusions))?;
    let seeded = seed::candidates(&dump, &excluded)?;
    check_counts(&seeded)?;

    let counts = &seeded.counts;
    let provenance = provenance_of(&args, &dump, &digest, counts);

    write_both(
        (&args.out, &registry_toml(&seeded.endpoints)),
        (&args.provenance, &provenance_toml(&provenance)),
    )?;
    tracing::info!(
        out = %args.out,
        provenance = %args.provenance,
        exclusions = %args.exclusions,
        datasets = counts.datasets,
        datasets_with_entries = counts.datasets_with_entries,
        entries = counts.entries,
        distinct = counts.distinct,
        refused_credentials = counts.refused_credentials,
        refused_excluded = counts.refused_excluded,
        refused_unroutable = counts.refused_unroutable,
        refused_reserved = counts.refused_reserved,
        refused_unpublishable = counts.refused_unpublishable,
        seeded = counts.seeded(),
        "wrote a candidate registry and its provenance"
    );
    Ok(())
}

/// Write both files, creating the directories they name, or leave both as they
/// were.
///
/// The directories, because the default paths put both files in a `registry/`
/// subdirectory and a first run in a fresh checkout would otherwise fail on the
/// second half of a path the caller did not choose.
///
/// Both rather than one at a time: `std::fs::write` truncates, so two
/// independent writes had a window in which the registry was regenerated and
/// the provenance beside it still described the previous dump, which is exactly
/// the state the digest gate exists to prevent. Both texts go to `.tmp` paths
/// first and both are renamed only after both writes have succeeded, so a full
/// disk or a read-only `registry/` fails before either destination is touched.
///
/// What this does NOT give is atomicity ACROSS the two renames: a crash between
/// them still leaves one new file beside one old one. That window is two
/// renames wide rather than two file writes wide, and closing it needs a
/// directory swap this tool has no reason to grow.
fn write_both(registry: (&str, &str), provenance: (&str, &str)) -> anyhow::Result<()> {
    let staged = [registry, provenance]
        .map(|(path, text)| (path.to_string(), format!("{path}.tmp"), text));
    for (path, tmp, text) in &staged {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(tmp, text)?;
    }
    for (path, tmp, _) in &staged {
        std::fs::rename(tmp, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests load a registry file back; `main` writes and never reads.
    use sparqlwatch_prober::registry;

    const SAMPLE: &[u8] = include_bytes!("../../tests/fixtures/lod-cloud-sample.json");
    const SHIPPED_REGISTRY: &str = include_str!("../../registry/lod-cloud.toml");
    const CALIBRATION_SAMPLE: &str = include_str!("../../registry/calibration-sample.toml");
    const SHIPPED_PROVENANCE: &str = include_str!("../../registry/lod-cloud.provenance.toml");

    /// The shipped exclusion list, which is what these tests load the shipped
    /// registry under.
    ///
    /// Not `&[]`: the fixed-point test below has to red when an exclusion lands
    /// on a host `lod-cloud.toml` still names, because the file then has to be
    /// re-seeded. Loading it under no exclusions at all would make that red
    /// disappear and leave the excluded host in a committed, public list.
    fn shipped_exclusions() -> Vec<registry::Exclusion> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(registry::DEFAULT_EXCLUSIONS);
        registry::read_exclusions(&path).expect("the shipped exclusion list must be readable")
    }

    fn shipped_provenance() -> Provenance {
        toml::from_str(SHIPPED_PROVENANCE).expect("the provenance file must parse")
    }

    /// The shipped list is a fixed point of write then read: what this binary
    /// renders for those endpoints is the file on disk, and what
    /// `registry::load_endpoints` reads back out of it is the same endpoints in
    /// the same order.
    ///
    /// This is the test that a quoting or escaping mistake cannot pass, across
    /// every URL in the file rather than a hand-picked few, two of which carry a
    /// query string and one a percent escape. It also catches a hand edit: the file is only
    /// byte-identical to the render if nobody has touched it since.
    ///
    /// What it CANNOT do is tie the file to the dump. The dump is 4 MB of a
    /// third party's data and is not committed, so nothing in this suite reads
    /// it, and this green covers the renderer and the loader rather than the
    /// extraction. That tie was checked by hand once, and the digest in
    /// `registry/lod-cloud.provenance.toml` is what makes it checkable again
    /// against the same bytes. `tests/seed_registry.rs` covers the extraction
    /// against a committed fixture instead.
    ///
    /// The list is asserted non-empty first because an empty one round trips
    /// trivially, so without that this would pass on a file that seeded
    /// nothing.
    #[test]
    fn the_shipped_registry_is_a_fixed_point_of_write_then_read() {
        let loaded = registry::load_endpoints(SHIPPED_REGISTRY, &shipped_exclusions()).unwrap();
        assert!(!loaded.is_empty(), "an empty list round trips trivially");
        let rendered = registry_toml(&loaded);
        assert_eq!(
            registry::load_endpoints(&rendered, &shipped_exclusions()).unwrap(),
            loaded,
            "the loader must read back exactly what this binary writes"
        );
        assert_eq!(
            rendered, SHIPPED_REGISTRY,
            "the shipped file must be what this binary writes for its own contents"
        );
    }

    /// A query string is where a URL stops being a bare path, and two of the
    /// 543 seeded candidates carry one: `.../query?query=` and `.../?g=LOD-a-lot`.
    /// Rendering has to leave `?`, `&`, `=` and a percent escape alone, and the
    /// loader has to hand them back unchanged: an endpoint string is published
    /// verbatim inside every subject about it, so a URL that came back
    /// re-spelled would be a fact about something else.
    #[test]
    fn a_url_carrying_a_query_string_round_trips_unchanged() {
        let tricky: Vec<String> = [
            "https://a.host.test/sparql?default-graph-uri=&query=SELECT+%2A",
            "https://b.host.test/sparql?a=1&b=2#frag",
            "https://c.host.test/api/sparql-endpoint-foodista",
            "https://d.host.test/sparql/",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let rendered = registry_toml(&tricky);
        assert_eq!(registry::load_endpoints(&rendered, &shipped_exclusions()).unwrap(), tricky);
    }

    /// The provenance parses, and it agrees with the list beside it: its
    /// `seeded` is that list's length, and it is also its own arithmetic.
    ///
    /// Two ways it can lie, both checked. It can name a total the list does not
    /// have, which is the case where somebody regenerated one file and not the
    /// other. And its own counts can fail to add up, which is the case where a
    /// refusal was applied without being counted, so `distinct` minus the
    /// refusals is not what the extractor kept.
    #[test]
    fn the_shipped_provenance_agrees_with_the_registry_beside_it() {
        let prov = shipped_provenance();
        let loaded = registry::load_endpoints(SHIPPED_REGISTRY, &shipped_exclusions()).unwrap();
        assert!(!loaded.is_empty(), "0 == 0 would agree with anything");
        assert_eq!(
            prov.counts.seeded,
            loaded.len(),
            "the provenance names a total the registry beside it does not have"
        );
        let c = &prov.counts;
        assert_eq!(
            c.seeded,
            c.distinct
                - c.refused_credentials
                - c.refused_excluded
                - c.refused_unroutable
                - c.refused_reserved
                - c.refused_unpublishable,
            "a refusal was applied without being counted"
        );
        assert!(c.distinct <= c.entries, "deduplication cannot add entries");
        assert!(
            c.datasets_with_entries <= c.datasets,
            "a dataset naming an endpoint is still a dataset"
        );
    }

    /// Every endpoint in the calibration sample is still in the registry it was
    /// cut from.
    ///
    /// `registry/calibration-sample.toml` is hand-cut, is loaded through
    /// `load_endpoints` like any other list, and had no test at all. What goes
    /// wrong is a re-seed: a candidate a newer dump stops naming leaves the
    /// registry while the sample still names it, and the costs measured on that
    /// sample are then attributed to a population that no longer contains it.
    /// The sample's own header states its composition, and its total is asserted
    /// here so a silently truncated file cannot pass the subset check trivially.
    #[test]
    fn every_calibration_endpoint_is_still_in_the_registry_it_was_cut_from() {
        let seeded = registry::load_endpoints(SHIPPED_REGISTRY, &shipped_exclusions()).unwrap();
        let sample = registry::load_endpoints(CALIBRATION_SAMPLE, &shipped_exclusions()).unwrap();
        assert_eq!(
            sample.len(),
            54,
            "the sample's header says 30 unknown plus 8 each from OK, timed out and \
             http_status"
        );
        let gone: Vec<&String> = sample.iter().filter(|e| !seeded.contains(e)).collect();
        assert!(
            gone.is_empty(),
            "the calibration sample names endpoints registry/lod-cloud.toml no longer \
             holds, so its measured costs describe a different population: {gone:?}"
        );
    }

    /// The recorded hash is only useful if the exact bytes can be fetched
    /// again, so the source has to be the versioned path. An unversioned URL is
    /// the case where the hash records what was used and nothing recovers it.
    ///
    /// The digest is checked for shape, not for a fixed value: a re-seed from a
    /// newer dump is supposed to change it, and pinning it here would make that
    /// a test failure instead of the intended update.
    #[test]
    fn the_provenance_identifies_a_dump_that_can_be_fetched_again_and_checked() {
        let prov = shipped_provenance();
        assert!(
            prov.dump.source.contains(&prov.dump.version),
            "the source must be the versioned path, so the hash identifies fetchable bytes: {} \
             does not name version {}",
            prov.dump.source,
            prov.dump.version
        );
        assert_eq!(prov.dump.sha256.len(), 64, "a SHA-256 is 64 hex digits");
        assert!(
            prov.dump.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "lowercase hex, as `shasum -a 256` prints it: {}",
            prov.dump.sha256
        );
        assert!(prov.dump.bytes > 0, "a dump of no bytes names no datasets");
        assert!(!prov.extraction.rule.is_empty(), "the rule has to be findable from here");
    }

    /// The provenance file is a fixed point of its own writer, symmetric with
    /// the registry's.
    ///
    /// Without this, `provenance_toml` is compared against nothing: the two
    /// tests below read the SHIPPED file and check its arithmetic and its shape,
    /// which are properties of the DATA, so the code that writes it could change
    /// or the file could be hand-edited with no red. A review of this branch
    /// proved that: rewriting `PROVENANCE_HEADER` survived, and so did dropping
    /// both headers from the render, which would have removed the one line that
    /// tells a reader the file is generated and not to edit it.
    ///
    /// This also promotes a wrong count in the provenance from "caught at the
    /// next commit" to "caught now": a `seeded` the writer would not produce
    /// fails here rather than waiting for somebody to re-seed.
    #[test]
    fn the_shipped_provenance_is_a_fixed_point_of_write_then_read() {
        let parsed = shipped_provenance();
        assert!(parsed.counts.seeded > 0, "an empty record round trips too easily");
        assert_eq!(
            provenance_toml(&parsed),
            SHIPPED_PROVENANCE,
            "the shipped provenance must be what this binary writes for its own contents"
        );
    }

    /// A dump that does not hash to the claim is refused, and the message names
    /// both digests.
    ///
    /// This is the gate the whole digest exists for. `main` had no test, and a
    /// review disabled the comparison with `if false &&` and watched all 351
    /// tests stay green: `--sha256 0000...` was then accepted and the tool wrote
    /// a provenance file whose `source`, `version` and `downloaded` described
    /// whatever the caller typed.
    #[test]
    fn a_dump_that_does_not_hash_to_the_claim_is_refused() {
        let digest = sha256_hex(b"abc");
        let wrong = "0".repeat(64);
        let error = check_digest("lod-data.json", &digest, &wrong)
            .expect_err("a dump that hashes to something else is not the dump named")
            .to_string();
        assert!(error.contains("lod-data.json"), "name the input: {error}");
        assert!(error.contains(&digest), "name what it hashes to: {error}");
        assert!(error.contains(&wrong), "name what was claimed: {error}");
        assert!(error.contains("Nothing was written"), "say what did not happen: {error}");

        check_digest("lod-data.json", &digest, &digest).expect("the right claim is accepted");
    }

    /// `--sha256` may be pasted in uppercase. The digest this crate computes is
    /// lowercase, so the claim is folded and not the other way round.
    #[test]
    fn an_uppercase_claim_names_the_same_dump() {
        let digest = sha256_hex(b"abc");
        check_digest("lod-data.json", &digest, &digest.to_ascii_uppercase())
            .expect("hex case does not change which bytes are named");
    }

    /// Counts that do not add up to the list are refused before anything is
    /// written.
    ///
    /// `seed::candidates` asserts this with `debug_assert`, which a release
    /// build compiles out, so this is the backstop. No dump can reach it, which
    /// is exactly why it needs a test: a review replaced the whole condition
    /// with `if false` and nothing went red, because the only way the suite
    /// could drive it was through `candidates`, where the arithmetic always
    /// agrees.
    #[test]
    fn counts_that_do_not_add_up_to_the_list_are_refused() {
        let honest = seed::Seeded {
            endpoints: vec!["https://a/sparql".to_string()],
            counts: seed::Counts { distinct: 1, ..seed::Counts::default() },
        };
        check_counts(&honest).expect("one distinct and one kept is one seeded");

        let uncounted = seed::Seeded {
            endpoints: vec!["https://a/sparql".to_string()],
            // Two distinct, one refused, and the refusal not counted: the
            // arithmetic says 2 while the list holds 1.
            counts: seed::Counts { distinct: 2, ..seed::Counts::default() },
        };
        let error = check_counts(&uncounted)
            .expect_err("2 by arithmetic against 1 kept is a refusal that went uncounted")
            .to_string();
        assert!(error.contains("2 seeded by arithmetic against 1"), "name both: {error}");
        assert!(error.contains("without being counted"), "name the cause: {error}");
    }

    /// The provenance describes the bytes that were read, and takes from the
    /// flags only what the bytes cannot say.
    ///
    /// `bytes` and `sha256` are the two fields a caller could get wrong without
    /// noticing, and a review found both mutable in silence: `bytes: 1` survived
    /// the whole suite, and so did writing `counts.distinct` into `seeded`,
    /// which on the real dump is 548 against the 543 the list holds. The digest
    /// passed in here deliberately differs from `--sha256`, so a record that
    /// echoed the flag would fail.
    #[test]
    fn the_provenance_records_the_dump_that_was_read_and_not_the_flags() {
        let dump = SAMPLE;
        let digest = sha256_hex(dump);
        let args = Args {
            dump: "lod-data.json".to_string(),
            // Not the digest of `dump`: nothing in the record may come from
            // here.
            sha256: "0".repeat(64),
            source: "https://lod-cloud.net/versions/2026-06-15/lod-data.json".to_string(),
            dump_version: "2026-06-15".to_string(),
            downloaded: "2026-08-19".to_string(),
            exclusions: sparqlwatch_prober::registry::DEFAULT_EXCLUSIONS.to_string(),
            out: "registry/lod-cloud.toml".to_string(),
            provenance: "registry/lod-cloud.provenance.toml".to_string(),
        };
        let seeded = seed::candidates(dump, &shipped_exclusions()).unwrap();
        let record = provenance_of(&args, dump, &digest, &seeded.counts);

        assert_eq!(record.dump.bytes, dump.len(), "the bytes read, not a constant");
        assert_eq!(record.dump.sha256, digest, "the digest read, not --sha256");
        assert_ne!(record.dump.sha256, args.sha256, "the flag is not the record");
        assert_eq!(
            record.counts.seeded,
            seeded.endpoints.len(),
            "the total the list has, not the distinct count before the refusals"
        );
        assert_ne!(
            record.counts.seeded, record.counts.distinct,
            "this fixture refuses four, so the two totals must differ for that \
             assertion to be about anything"
        );
        assert_eq!(record.dump.source, args.source, "only the caller knows where it came from");
        assert_eq!(record.dump.version, args.dump_version);
        assert_eq!(record.dump.downloaded, args.downloaded);
        assert_eq!(record.extraction.crate_version, env!("CARGO_PKG_VERSION"));
    }

    /// The digest is computed here rather than copied from a flag, so it has to
    /// be right. These are the published vectors: the empty input, one shorter
    /// than a block, one that pads into a second block, and one longer than a
    /// block.
    ///
    /// The third is the case a hand-written padding gets wrong. A 56-byte input
    /// leaves no room for the length in its own block, so the length goes in a
    /// block of its own, and an implementation that pads to 56 modulo 64
    /// without that case produces a digest for the wrong message.
    #[test]
    fn the_digest_matches_the_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            "56 bytes: the length does not fit in this block and pads into another"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 1000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3",
            "many blocks"
        );
    }
}

