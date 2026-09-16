//! The seeder, run as a process against a fixture dump.
//!
//! `seed-registry`'s `main` is where the two gates are wired: the digest
//! comparison that ties the provenance file to the dump beside it, and the
//! counts check that backstops a `debug_assert` a release build removes. A
//! review of stage 1d-a measured what leaving `main` untested cost: disabling
//! the digest comparison with `if false &&`, writing `counts.distinct` into
//! `seeded`, and replacing `bytes: dump.len()` with `bytes: 1` each left all 351
//! tests green. Those three are now killed by unit tests in the binary itself;
//! what only a process can answer is whether `main` still CALLS them, and
//! whether the two files it writes are the ones it rendered.
//!
//! `prober/tests/binary.rs` is the precedent, and for the same reason:
//! `env!("CARGO_BIN_EXE_...")` is the built binary's path, which cargo sets for
//! integration targets, so running the real process needs no dependency beyond
//! what the suite already has.
//!
//! No timeout guard here, unlike `binary.rs`. That file bounds its wait because
//! a sweep can hang on a network read; this binary reads one 3 KB file, hashes
//! it, and writes two more, with no socket and no runtime anywhere in it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SAMPLE: &[u8] = include_bytes!("fixtures/lod-cloud-sample.json");

/// The SHA-256 of `tests/fixtures/lod-cloud-sample.json`, from `shasum -a 256`.
///
/// Pinned from outside this crate on purpose: the gate under test compares the
/// digest this crate computes against a claim, and checking it against a claim
/// this crate also computed would test the comparison against itself. Editing
/// the fixture changes this value, and the assertions below name it when they
/// disagree.
const SAMPLE_SHA256: &str = "8eeab4e60ac258c64fd81e7f04e978685c87c3ee6a13934dfe8645b6e4adbd12";

/// A digest no file has, for the mismatch case.
const NOT_THE_DUMP: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The shipped exclusion list, which is what a seed applies unless a test says
/// otherwise. Found through `CARGO_MANIFEST_DIR` and not through the binary's
/// default, which is relative to the working directory.
fn shipped_exclusions_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(sparqlwatch_prober::registry::DEFAULT_EXCLUSIONS)
}

fn shipped_exclusions() -> Vec<sparqlwatch_prober::registry::Exclusion> {
    sparqlwatch_prober::registry::read_exclusions(&shipped_exclusions_path())
        .expect("the shipped exclusion list must be readable")
}

/// Where the fixture lives on disk, since the binary reads a path and not bytes.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lod-cloud-sample.json")
}

/// A fresh directory under the target dir cargo already owns, named for the
/// caller and this process, so two tests in this file cannot overwrite each
/// other's output.
fn tempdir(named: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("seed-{named}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the target tmpdir must be writable");
    dir
}

/// Both destinations are renamed only after BOTH writes succeed, so a failure
/// on the second write leaves neither in place.
///
/// The window this closes is not hypothetical: `std::fs::write` truncates, so
/// two independent writes had a state in which the registry described the new
/// dump and the provenance beside it still described the old one, which is
/// exactly what the digest gate exists to prevent.
///
/// Provoked by planting a DIRECTORY where the provenance's `.tmp` file has to
/// go. A directory cannot be overwritten by a file write, so the second staged
/// write fails while the first has already succeeded, which is the only shape
/// that distinguishes renaming-after-both from renaming-as-you-go. Moving the
/// rename inside the write loop leaves the registry file behind and this test
/// is what notices.
#[test]
fn a_second_write_that_fails_leaves_neither_destination_written() {
    let dir = tempdir("staged");
    let out = dir.join("registry/lod-cloud.toml");
    let prov = dir.join("registry/lod-cloud.provenance.toml");
    std::fs::create_dir_all(prov.parent().unwrap()).unwrap();
    // The seeder stages `<path>.tmp` beside each destination.
    std::fs::create_dir_all(format!("{}.tmp", prov.display())).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seed-registry"))
        .args(["--exclusions", shipped_exclusions_path().to_str().unwrap()])
        .args(["--dump", fixture().to_str().unwrap(), "--sha256", SAMPLE_SHA256,
               "--source", "https://lod-cloud.net/versions/2026-06-15/lod-data.json",
               "--dump-version", "2026-06-15", "--downloaded", "2026-08-19",
               "--out", out.to_str().unwrap(), "--provenance", prov.to_str().unwrap()])
        .output()
        .expect("the seeder binary must run");

    assert!(!output.status.success(), "a failed staged write must not report success");
    assert!(
        !out.exists(),
        "the registry was renamed into place while the provenance could not be written, so \
         the two files would describe different dumps"
    );
    assert!(!prov.exists(), "and the provenance itself must not appear");
}

/// Run the seeder over the fixture, claiming `sha256`, writing into `dir`.
///
/// The output paths are nested one level deeper than `dir` so that the
/// directory-creating half of `write_both` is exercised too: the real invocation
/// writes into a `registry/` subdirectory that a fresh checkout may not have.
fn seed(dir: &Path, sha256: &str, exclusions: &Path) -> (Output, PathBuf, PathBuf) {
    let out = dir.join("registry/lod-cloud.toml");
    let provenance = dir.join("registry/lod-cloud.provenance.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_seed-registry"))
        .args(["--dump", fixture().to_str().unwrap()])
        .args(["--sha256", sha256])
        .args(["--exclusions", exclusions.to_str().unwrap()])
        .args(["--source", "https://lod-cloud.net/versions/2026-06-15/lod-data.json"])
        .args(["--dump-version", "2026-06-15"])
        .args(["--downloaded", "2026-08-19"])
        .args(["--out", out.to_str().unwrap()])
        .args(["--provenance", provenance.to_str().unwrap()])
        .output()
        .expect("the built binary must be runnable");
    (output, out, provenance)
}

/// A claim that does not match the dump stops the run, and nothing is written.
///
/// This is the gate the tool computes its own digest for. The message has to
/// name the digest it actually read, because that is how the caller fixes the
/// command line, and it is also what pins the digest to a value `shasum`
/// produced rather than to whatever this crate happens to compute.
#[test]
fn a_dump_that_does_not_hash_to_the_claim_writes_nothing() {
    let dir = tempdir("mismatch");
    let (output, out, provenance) = seed(&dir, NOT_THE_DUMP, &shipped_exclusions_path());

    assert!(
        !output.status.success(),
        "a dump that is not the one named must not seed a registry, exited {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not the dump named on the command line"),
        "the failure must say which check refused it: {stderr}"
    );
    assert!(
        stderr.contains(SAMPLE_SHA256),
        "the message must name the digest the file really has, {SAMPLE_SHA256}: {stderr}"
    );
    assert!(
        stderr.contains(NOT_THE_DUMP),
        "and the claim it was compared against: {stderr}"
    );
    assert!(!out.exists(), "no registry may be written: {}", out.display());
    assert!(
        !provenance.exists(),
        "and no provenance: {}",
        provenance.display()
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The right claim seeds both files, and both describe the dump that was read.
///
/// Every assertion here is against the library's own answer for the same bytes
/// rather than against a transcribed list, so this cannot drift from
/// `seed::candidates`, and against `dump.len()` and `SAMPLE_SHA256` rather than
/// against the flags, which is the property the provenance file exists to have.
#[test]
fn the_right_claim_seeds_two_files_that_describe_the_dump_read() {
    let dir = tempdir("seeded");
    let (output, out, provenance) = seed(&dir, SAMPLE_SHA256, &shipped_exclusions_path());
    assert!(
        output.status.success(),
        "the fixture's own digest must be accepted, exited {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = sparqlwatch_prober::seed::candidates(SAMPLE, &shipped_exclusions()).unwrap();
    assert!(!expected.endpoints.is_empty(), "an empty list would agree with anything");

    let written = std::fs::read_to_string(&out).expect("the registry must be written");
    let expected_urls: Vec<String> = expected.endpoints.iter().map(|e| e.url.clone()).collect();
    assert_eq!(
        sparqlwatch_prober::registry::load_endpoints(&written, &shipped_exclusions()).unwrap(),
        expected_urls,
        "the registry has to be the candidates the extractor found, in that order"
    );
    assert!(
        written.starts_with("# Generated by"),
        "a generated file has to say so: {written}"
    );

    let text = std::fs::read_to_string(&provenance).expect("the provenance must be written");
    assert!(
        text.starts_with("# Generated by"),
        "a generated file has to say so: {text}"
    );
    let record: toml::Value = toml::from_str(&text).expect("the provenance must parse");
    let integer = |table: &str, key: &str| {
        record[table][key]
            .as_integer()
            .unwrap_or_else(|| panic!("{table}.{key} must be an integer: {text}")) as usize
    };
    let string = |table: &str, key: &str| {
        record[table][key]
            .as_str()
            .unwrap_or_else(|| panic!("{table}.{key} must be a string: {text}"))
            .to_string()
    };
    assert_eq!(string("dump", "sha256"), SAMPLE_SHA256, "the digest of the bytes read");
    assert_eq!(integer("dump", "bytes"), SAMPLE.len(), "the length of the bytes read");
    assert_eq!(
        integer("counts", "seeded"),
        expected.endpoints.len(),
        "the total the registry beside it has"
    );
    assert_eq!(integer("counts", "distinct"), expected.counts.distinct);
    assert_ne!(
        integer("counts", "seeded"),
        integer("counts", "distinct"),
        "this fixture refuses four candidates, so the two totals must differ for the \
         assertion above to be about anything"
    );
    assert_eq!(string("dump", "version"), "2026-06-15", "the caller's word, not the bytes'");
    assert_eq!(string("dump", "downloaded"), "2026-08-19");

    // `write_both` stages through `.tmp` paths and renames. A leftover means a
    // rename did not happen, which is the half-written state it exists to avoid.
    for path in [&out, &provenance] {
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        assert!(!tmp.exists(), "a staging file was left behind: {}", tmp.display());
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A re-seed applies the exclusion list, and leaves it byte for byte.
///
/// Both halves in one test, because each is only worth something with the
/// other. Applying the list is what keeps an excluded host out of the
/// COMMITTED artefact, which is public and is regenerated from the dump; not
/// writing the file is what makes an entry outlive the re-seed that undoes a
/// hand deletion from `lod-cloud.toml`.
///
/// The excluded host is a real candidate of the fixture dump, so the assertions
/// are about a URL the seeder would otherwise have written: the list this
/// invocation is given names `linked.opendata.cz`, and `refused_excluded` in
/// the provenance beside the registry has to account for it. The earlier
/// version of this test planted a file nothing read and asserted it was still
/// there, which could not fail; this one reds if the seeder stops applying the
/// list, stops counting the drop, or rewrites the file it was handed.
#[test]
fn a_re_seed_applies_the_exclusion_list_and_leaves_it_byte_for_byte() {
    let dir = tempdir("excluded");
    let list = dir.join("registry/exclusions.toml");
    std::fs::create_dir_all(list.parent().unwrap()).unwrap();
    let written_list = "# a hand-written list, which the seeder reads and never writes\n\n                        [[exclusion]]\nhost = \"linked.opendata.cz\"\nreason = \"a person                         asked, 2026-08-25\"\n";
    std::fs::write(&list, written_list).unwrap();

    let excluded = "http://linked.opendata.cz/sparql";
    let unrestricted = sparqlwatch_prober::seed::candidates(SAMPLE, &[]).unwrap();
    assert!(
        unrestricted.endpoints.iter().any(|e| e.url == excluded),
        "the fixture has to name {excluded} for this test to be about anything"
    );

    let (output, out, provenance) = seed(&dir, SAMPLE_SHA256, &list);
    assert!(
        output.status.success(),
        "the seed has to succeed for its output to be worth reading: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let registry = std::fs::read_to_string(&out).expect("the registry must be written");
    assert!(
        !registry.contains(excluded),
        "an excluded host may not be written into a committed, public list: {registry}"
    );
    assert!(
        registry.contains("http://ja.dbpedia.org/sparql"),
        "only the excluded host may be missing: {registry}"
    );

    let record: toml::Value =
        toml::from_str(&std::fs::read_to_string(&provenance).unwrap()).unwrap();
    assert_eq!(
        record["counts"]["refused_excluded"].as_integer(),
        Some(1),
        "the drop has to be counted under its own reason: {record}"
    );
    assert_eq!(
        record["counts"]["seeded"].as_integer(),
        Some(unrestricted.endpoints.len() as i64 - 1),
        "one candidate fewer than the same dump seeds under no exclusions: {record}"
    );

    assert_eq!(
        std::fs::read_to_string(&list).unwrap(),
        written_list,
        "the seeder must not rewrite the exclusion list it reads"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A seed whose exclusion list cannot be read writes nothing, and says which
/// path it could not read.
///
/// The file is read at run time, so this state exists, and it is the state in
/// which a registry would otherwise be written as though nobody had ever asked
/// to be left out of it. There is no way to tell such a file apart from one
/// seeded with the list applied, which is why this fails rather than warning.
#[test]
fn a_seed_whose_exclusion_list_cannot_be_read_writes_nothing() {
    let dir = tempdir("no-list");
    let missing = dir.join("registry/absent-exclusions.toml");
    let (output, out, provenance) = seed(&dir, SAMPLE_SHA256, &missing);

    assert!(
        !output.status.success(),
        "a seed that cannot read the exclusion list must not report success, exited {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("absent-exclusions.toml"),
        "the failure must name the path it could not read: {stderr}"
    );
    assert!(!out.exists(), "no registry may be written: {}", out.display());
    assert!(!provenance.exists(), "and no provenance: {}", provenance.display());

    std::fs::remove_dir_all(&dir).unwrap();
}
