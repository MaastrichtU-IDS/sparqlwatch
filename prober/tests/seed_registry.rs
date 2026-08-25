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
fn seed(dir: &Path, sha256: &str) -> (Output, PathBuf, PathBuf) {
    let out = dir.join("registry/lod-cloud.toml");
    let provenance = dir.join("registry/lod-cloud.provenance.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_seed-registry"))
        .args(["--dump", fixture().to_str().unwrap()])
        .args(["--sha256", sha256])
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
    let (output, out, provenance) = seed(&dir, NOT_THE_DUMP);

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
    let (output, out, provenance) = seed(&dir, SAMPLE_SHA256);
    assert!(
        output.status.success(),
        "the fixture's own digest must be accepted, exited {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = sparqlwatch_prober::seed::candidates(SAMPLE).unwrap();
    assert!(!expected.endpoints.is_empty(), "an empty list would agree with anything");

    let written = std::fs::read_to_string(&out).expect("the registry must be written");
    assert_eq!(
        sparqlwatch_prober::registry::load_endpoints(&written).unwrap(),
        expected.endpoints,
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

/// A re-seed preserves `registry/exclusions.toml`.
///
/// The property the exclusion mechanism rests on. `seed-registry` regenerates
/// the registry, so removing a host from `lod-cloud.toml` by hand lasts until
/// the next seed; the exclusion file is what outlives one, and it can only do
/// that if the seeder never writes it. There is no flag naming it and no code
/// path that opens it for writing, which is a claim about the absence of code
/// and therefore one a test has to hold rather than a reader.
///
/// Both copies are checked. The one in the tempdir sits in the same `registry/`
/// directory the seeder writes its two outputs into, which is where a future
/// "regenerate everything in this directory" would clobber it. The shipped one
/// is the file that actually matters, and this invocation runs with the crate
/// root as its working directory, so a default `--out` would have reached it.
#[test]
fn a_re_seed_leaves_the_exclusion_list_exactly_as_it_was() {
    let dir = tempdir("exclusions");
    let beside = dir.join("registry/exclusions.toml");
    std::fs::create_dir_all(beside.parent().unwrap()).unwrap();
    let planted = "# planted by a_re_seed_leaves_the_exclusion_list_exactly_as_it_was\n";
    std::fs::write(&beside, planted).unwrap();

    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("registry/exclusions.toml");
    let before = std::fs::read(&shipped).expect("the shipped exclusion list must exist");
    assert!(!before.is_empty(), "an empty file would be preserved trivially");

    let (output, out, provenance) = seed(&dir, SAMPLE_SHA256);
    assert!(
        output.status.success(),
        "the seed itself has to succeed for this to be about the exclusion file: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.exists() && provenance.exists(), "both outputs must have been written");

    assert_eq!(
        std::fs::read_to_string(&beside).unwrap(),
        planted,
        "a file in the directory the seeder writes into was rewritten"
    );
    assert_eq!(
        std::fs::read(&shipped).unwrap(),
        before,
        "the shipped exclusion list must survive a re-seed byte for byte"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
