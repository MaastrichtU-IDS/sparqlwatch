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
/// arithmetic: `distinct` minus the four refusals.
#[derive(Serialize, Deserialize)]
struct CountsRecord {
    datasets: usize,
    datasets_with_entries: usize,
    entries: usize,
    distinct: usize,
    refused_credentials: usize,
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

    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (word, raw) in w.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(raw.try_into().expect("chunks_exact(4) yields four bytes"));
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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let dump = std::fs::read(&args.dump)?;

    // Before anything is extracted: a list seeded from a dump other than the
    // one the provenance file will name is a list whose provenance is wrong,
    // and no later check could tell.
    let digest = sha256_hex(&dump);
    let expected = args.sha256.to_ascii_lowercase();
    if digest != expected {
        anyhow::bail!(
            "{} is not the dump named on the command line: it hashes to {digest}, not to \
             {expected}. Nothing was written, because a registry seeded from one dump and a \
             provenance file naming another cannot be told apart afterwards.",
            args.dump,
        );
    }

    let seeded = seed::candidates(&dump)?;
    // `seed::candidates` states this as a `debug_assert`, which is compiled out
    // of a release build. Checked again here because this is where the number
    // becomes a permanent claim in a file, and the two files would then disagree
    // about a total neither produced.
    if seeded.counts.seeded() != seeded.endpoints.len() {
        anyhow::bail!(
            "the counts do not add up: {} seeded by arithmetic against {} candidates kept, so a \
             refusal was applied without being counted",
            seeded.counts.seeded(),
            seeded.endpoints.len(),
        );
    }

    let counts = &seeded.counts;
    let provenance = Provenance {
        dump: Dump {
            source: args.source,
            version: args.dump_version,
            downloaded: args.downloaded,
            // From the bytes read, not from a flag, for the same reason as the
            // digest.
            bytes: dump.len(),
            sha256: digest,
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
            refused_unroutable: counts.refused_unroutable,
            refused_reserved: counts.refused_reserved,
            refused_unpublishable: counts.refused_unpublishable,
            seeded: counts.seeded(),
        },
    };

    write_beside(&args.out, &registry_toml(&seeded.endpoints))?;
    write_beside(&args.provenance, &provenance_toml(&provenance))?;
    tracing::info!(
        out = %args.out,
        provenance = %args.provenance,
        datasets = counts.datasets,
        datasets_with_entries = counts.datasets_with_entries,
        entries = counts.entries,
        distinct = counts.distinct,
        refused_credentials = counts.refused_credentials,
        refused_unroutable = counts.refused_unroutable,
        refused_reserved = counts.refused_reserved,
        refused_unpublishable = counts.refused_unpublishable,
        seeded = counts.seeded(),
        "wrote a candidate registry and its provenance"
    );
    Ok(())
}

/// Write `text` to `path`, creating the directory it names if it is missing.
///
/// The directory, because the default paths put both files in a `registry/`
/// subdirectory and a first run in a fresh checkout would otherwise fail on the
/// second half of a path the caller did not choose.
fn write_beside(path: &str, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests load a registry file back; `main` writes and never reads.
    use sparqlwatch_prober::registry;

    const SHIPPED_REGISTRY: &str = include_str!("../../registry/lod-cloud.toml");
    const SHIPPED_PROVENANCE: &str = include_str!("../../registry/lod-cloud.provenance.toml");

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
    /// The list is asserted non-empty first because an empty one round trips
    /// trivially, so without that this would pass on a file that seeded
    /// nothing.
    #[test]
    fn the_shipped_registry_is_a_fixed_point_of_write_then_read() {
        let loaded = registry::load_endpoints(SHIPPED_REGISTRY).unwrap();
        assert!(!loaded.is_empty(), "an empty list round trips trivially");
        let rendered = registry_toml(&loaded);
        assert_eq!(
            registry::load_endpoints(&rendered).unwrap(),
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
        assert_eq!(registry::load_endpoints(&rendered).unwrap(), tricky);
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
        let loaded = registry::load_endpoints(SHIPPED_REGISTRY).unwrap();
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

