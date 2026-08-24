//! Where a run's bytes go while the sweep is still running, and why they do
//! not go straight to `--out`.
//!
//! `emit` says what a run's sections are; this module owns the file they are
//! written to and the order they reach it in. Three rules, each of which exists
//! because the alternative loses data an operator cannot get back.
//!
//! 1. **A run in progress is written to a sibling of `--out`, and renamed onto
//!    it at the end.** `--out` is the source of truth for the loaded store
//!    (`web/load_run.py` says so in as many words), and nothing re-creates last
//!    night's file, so opening `--out` for writing at t=0 would mean a sweep
//!    that crashed at endpoint 500 of 548 had destroyed the previous complete
//!    run. `rename` within one directory is atomic on macOS and Linux, so
//!    `--out` is always either the previous complete run or this one.
//! 2. **The sibling is named for the run**, `<out>.<at>.partial`, not a fixed
//!    `<out>.partial`. A fixed suffix means the next scheduled sweep truncates
//!    the previous crash's partial file on its first write, which is the same
//!    loss displaced by one run. `--at` is required and validated by `main.rs`
//!    before anything is opened, so it is a name-safe label that is unique per
//!    run. A run whose partial file is already there is **refused**, not
//!    overwritten: a retry shares the `--at`, so overwriting would move that same
//!    loss onto the retry, which is the documented recovery path.
//!    `renamed onto --out` is not the same as `written through --out`: a `--out`
//!    that is a symlink is REPLACED by the finished file, where the `fs::write`
//!    this module took over from followed it. A deployment that points `--out` at
//!    a symlink into a mounted volume has to point it at the real path instead.
//! 3. **Every chunk is flushed as it is written, and `Drop` is not relied on for
//!    any of it.** A `SIGKILL` runs no destructors, and the crash this module
//!    exists for is the one where nothing gets to run. The buffer is sized for a
//!    whole chunk so the flush is one write per endpoint rather than several:
//!    the committed `web/tests/fixtures/run-with-samples.nq` spends 19,692 bytes
//!    on 109 `sampledValue` lines, and `metrics.toml`'s `classes` declares
//!    `sample_limit = 200`, so one chunk can be roughly 40 KB, well over
//!    `BufWriter`'s 8 KB default.

use crate::emit::{
    emit_endpoint, emit_footer, emit_header, EmitState, EndpointFacts, RunFooter, RunHeader, RunId,
};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Room for one whole chunk, so an endpoint's facts reach the file in one
/// write rather than in five. See rule 3 above for where 40 KB comes from; this
/// is the next power of two above it.
pub const CHUNK_BUFFER_BYTES: usize = 65_536;

/// The file a run in progress is written to: `<out>.<at>.partial`.
///
/// A free function rather than a private detail of the constructor, because
/// three other places have to agree with it: `main.rs`'s log line, the README's
/// statement of the guarantee, and an operator loading what a crashed sweep
/// left behind.
pub fn partial_path(out: &Path, at: &str) -> PathBuf {
    let mut name = out.as_os_str().to_os_string();
    name.push(format!(".{at}.partial"));
    PathBuf::from(name)
}

/// One run's output file, written a section at a time.
///
/// Generic over the sink so a test can inject a writer that fails partway
/// through a sweep. That is not the same failure as an unwritable path, which
/// `create` reports before a single byte is written; the interesting case is the
/// disk filling up after 300 endpoints, and only an injected sink reaches it.
pub struct RunWriter<W: Write> {
    sink: W,
    /// The run every chunk hangs off, as the header fixed it.
    ///
    /// `EndpointFacts` carries a `RunId` of its own and `emit_endpoint` derives
    /// the graph name from that one, so this field does not decide where a
    /// chunk lands. What it does is let `write_endpoint` refuse a chunk built
    /// for a different run, which would otherwise be a fact about a run this
    /// file does not describe, in a second named graph in the same file. It is
    /// also what `run()` hands a caller so it can build facts that match.
    run: RunId,
    /// `emit`'s cross-chunk state: which endpoints it has been asked for.
    state: EmitState,
    /// The same set, kept here because the two layers do different things with
    /// it. `emit_endpoint` DROPS a repeat chunk, which is right for a function
    /// that returns a string its caller may discard; a writer whose earlier
    /// chunk is already on disk cannot drop anything, so it refuses instead and
    /// names the endpoint. The check below runs first, so `emit`'s own arm is
    /// unreachable through this path and stays as the guard for a caller that
    /// composes the sections itself.
    written: BTreeSet<String>,
    /// Where `finish` renames the file, and from where. `None` for a writer over
    /// an injected sink, which owns no path to rename.
    rename: Option<(PathBuf, PathBuf)>,
}

impl RunWriter<BufWriter<File>> {
    /// Open `<out>.<at>.partial` and write the header into it.
    ///
    /// Three refusals happen here, before any probing, so an operator learns
    /// about an unusable destination at t=0 rather than after a ten-minute sweep:
    /// a partial file for this `--at` that already exists, a `--out` that is a
    /// directory, and any other reason the partial cannot be created.
    pub fn create(out: &Path, at: &str, header: RunHeader) -> anyhow::Result<Self> {
        let partial = partial_path(out, at);
        // A directory `--out` would take the partial file happily, since that is
        // a sibling NAME, and then fail in `finish` where the rename lands on the
        // directory. Checked here instead, because the whole point of failing in
        // the constructor is that it costs no probing.
        if out.is_dir() {
            anyhow::bail!(
                "cannot write {}: it is a directory, and a finished run is renamed onto it",
                out.display()
            );
        }
        // `create_new`, so a partial file that is already there is REFUSED rather
        // than truncated. The `<at>` label stops the next scheduled sweep
        // destroying a crashed run's work; without this it would be destroyed by
        // the retry instead, which shares the `--at` by design (see the `--at`
        // paragraph in README.md) and is the documented recovery path. A retry
        // has no prior on getting further than the attempt that crashed, so the
        // 500 endpoints already on disk are not this process's to discard: the
        // refusal is loud, costs nothing but a rerun, and leaves the decision
        // where it belongs.
        //
        // It is also `O_EXCL`, so two invocations that share an `--at` cannot
        // interleave into one file: the second is refused rather than writing a
        // second header into the middle of the first one's run.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => anyhow::anyhow!(
                    "{} already exists, so an earlier run of {at} did not finish. This run \
                     would overwrite it, and it may hold every endpoint that run reached. \
                     Load it (python web/load_run.py STORE {}), or move it aside, then run \
                     again.",
                    partial.display(),
                    partial.display(),
                ),
                _ => anyhow::anyhow!("cannot write {}: {e}", partial.display()),
            })?;
        let sink = BufWriter::with_capacity(CHUNK_BUFFER_BYTES, file);
        Self::start(sink, header, Some((partial, out.to_path_buf())))
    }
}

impl<W: Write> RunWriter<W> {
    /// A writer over any sink, with no file and nothing to rename. For tests,
    /// and for a caller that wants the sections somewhere other than a file.
    pub fn with_writer(sink: W, header: RunHeader) -> anyhow::Result<Self> {
        Self::start(sink, header, None)
    }

    fn start(
        sink: W,
        header: RunHeader,
        rename: Option<(PathBuf, PathBuf)>,
    ) -> anyhow::Result<Self> {
        let run = RunId(header.run.0.clone());
        let mut writer = Self {
            sink,
            run,
            state: EmitState::new(),
            written: BTreeSet::new(),
            rename,
        };
        // The header goes out and is flushed before any probing starts, so a
        // sweep killed before its first endpoint finished still leaves a file
        // `load_run.py` can take: the header's terminator is the earliest of
        // the three it cuts back to, and the run's activity is what the read
        // queries join everything else onto.
        let bytes = emit_header(header)?;
        writer.sink.write_all(bytes.as_bytes())?;
        writer.sink.flush()?;
        Ok(writer)
    }

    /// The run this file describes, so a caller building `EndpointFacts` cannot
    /// name a different one.
    pub fn run(&self) -> &RunId {
        &self.run
    }

    /// Write one endpoint's chunk and flush it.
    ///
    /// Refuses a second chunk for an endpoint it has already written, naming
    /// the endpoint. That refusal is what turns "an endpoint's facts are all in
    /// one chunk" from a convention into a checked invariant, and `emit`'s
    /// per-chunk duplicate pre-scan rests on it: two facts about one (endpoint,
    /// metric) pair either land in this chunk, where the pre-scan sees both, or
    /// nowhere.
    ///
    /// Refuses a chunk built for another run too, before recording the endpoint
    /// as written: `emit_endpoint` takes the graph name from `facts.run`, so
    /// such a chunk would open a second named graph in a file whose header
    /// describes one run, and `load_run.py` replaces every graph a file names,
    /// leaving an orphan chunk in a run graph with no header.
    pub fn write_endpoint(&mut self, facts: EndpointFacts) -> anyhow::Result<()> {
        if facts.run.0 != self.run.0 {
            anyhow::bail!(
                "refusing a chunk for run {} in a file whose header describes run {}: one file \
                 is one run",
                facts.run.0,
                self.run.0
            );
        }
        if !self.written.insert(facts.endpoint.to_string()) {
            anyhow::bail!(
                "refusing a second chunk for {}: every fact about an endpoint has to be in one \
                 chunk, and the first one is already written",
                facts.endpoint
            );
        }
        let bytes = emit_endpoint(&mut self.state, facts)?;
        self.sink.write_all(bytes.as_bytes())?;
        // Once per chunk, and not left to `Drop`: a `SIGKILL` runs no
        // destructors, so a chunk that is only in the buffer is a chunk that
        // does not exist. The buffer holds a whole chunk (see
        // `CHUNK_BUFFER_BYTES`), so this is one write per endpoint.
        self.sink.flush()?;
        Ok(())
    }

    /// Write the footer, flush, and rename the file onto `--out`.
    ///
    /// Consumes the writer, because a section after the footer's terminator
    /// would be a fact published after the file said the run was over.
    pub fn finish(mut self, footer: RunFooter) -> anyhow::Result<()> {
        let bytes = emit_footer(footer)?;
        self.sink.write_all(bytes.as_bytes())?;
        // Flushed BEFORE the rename, not left to the drop that follows it:
        // renaming a file whose last section is still in a buffer would put a
        // run at `--out` whose footer arrives afterwards or not at all, and the
        // footer is the only thing that says the run finished.
        self.sink.flush()?;
        if let Some((from, to)) = &self.rename {
            std::fs::rename(from, to).map_err(|e| {
                anyhow::anyhow!("cannot rename {} onto {}: {e}", from.display(), to.display())
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::{
        ContentSample, DeclarationsRead, MeasurementRow, NotMeasured, NotMeasuredReason,
    };
    use crate::metrics::Cost;
    use crate::verdict::Verdict;
    use oxrdf::{Quad, Term};
    use oxrdfio::{RdfFormat, RdfParser};
    use std::num::NonZeroUsize;
    use std::sync::{Arc, Mutex};

    const AT: &str = "2026-08-20T08:00:00Z";
    const EP: &str = "https://a.example/sparql";

    fn header<'a>(run: &'a RunId) -> RunHeader<'a> {
        RunHeader {
            run,
            generated_at: AT,
            metric_revision: "test-revision",
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
        }
    }

    /// One endpoint's facts, one of each family, so a chunk built from them
    /// carries every join the three read queries make.
    fn facts_for<'a>(run: &'a RunId, endpoint: &'a str, all: &'a Families) -> EndpointFacts<'a> {
        EndpointFacts {
            run,
            endpoint,
            rows: &all.rows,
            declarations_read: &all.declarations_read,
            not_measured: &all.not_measured,
            content_samples: &all.content_samples,
        }
    }

    struct Families {
        rows: Vec<MeasurementRow>,
        declarations_read: Vec<DeclarationsRead>,
        not_measured: Vec<NotMeasured>,
        content_samples: Vec<ContentSample>,
    }

    /// The metric ids are the shipped ones on purpose: `web/queries/`
    /// `endpoint_content.rq` joins on `sw:metric:classes` by name, so a sample
    /// under any other id would not answer that query at all.
    fn families(endpoint: &str) -> Families {
        Families {
            rows: vec![MeasurementRow {
                endpoint: endpoint.into(),
                metric_id: "availability".into(),
                verdict: Verdict::Verified,
                level: None,
                elapsed_ms: Some(12),
            }],
            declarations_read: vec![DeclarationsRead { endpoint: endpoint.into(), read: true }],
            not_measured: vec![NotMeasured {
                endpoint: endpoint.into(),
                metric_id: "geo-data".into(),
                reason: NotMeasuredReason::CostCeiling,
            }],
            content_samples: vec![ContentSample {
                endpoint: endpoint.into(),
                metric_id: "classes".into(),
                values: vec!["https://a.example/vocab#Zebra".into()],
                truncated: false,
            }],
        }
    }

    fn quads_of(out: &[u8]) -> Vec<Quad> {
        RdfParser::from_format(RdfFormat::NQuads)
            .for_slice(out)
            .map(|q| q.expect("what the writer wrote must parse as N-Quads"))
            .collect()
    }

    /// The single object of one (subject, predicate), or None. Written as a
    /// lookup rather than a filter because every assertion below is a join: the
    /// question is always whether the subject a query arrives at carries the
    /// predicate the query then follows.
    fn object<'a>(qs: &'a [Quad], subject: &str, predicate: &str) -> Option<&'a Term> {
        qs.iter()
            .find(|q| q.subject.to_string().trim_matches(['<', '>']) == subject
                && q.predicate.as_str() == predicate)
            .map(|q| &q.object)
    }

    fn subjects_of(qs: &[Quad], predicate: &str, object: &str) -> Vec<String> {
        qs.iter()
            .filter(|q| q.predicate.as_str() == predicate && q.object.to_string() == object)
            .map(|q| q.subject.to_string().trim_matches(['<', '>']).to_string())
            .collect()
    }

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const DQV: &str = "http://www.w3.org/ns/dqv#";
    const PROV: &str = "http://www.w3.org/ns/prov#";

    /// A sink whose bytes stay readable after the writer that owns it is gone,
    /// so a test can inspect what a writer wrote without the writer having to
    /// hand its sink back.
    #[derive(Clone)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A header plus one chunk, which is what a reader of a crashed run holds.
    fn header_and_one_chunk() -> Vec<u8> {
        let run = RunId("r1".into());
        let all = families(EP);
        let sink = Shared(Arc::new(Mutex::new(Vec::new())));
        let bytes = Arc::clone(&sink.0);
        let mut w = RunWriter::with_writer(sink, header(&run)).unwrap();
        w.write_endpoint(facts_for(&run, EP, &all)).unwrap();
        // `finish` is deliberately not called: the document under test is the
        // one a crash leaves, header plus chunk and no footer.
        drop(w);
        let written = bytes.lock().unwrap().clone();
        written
    }

    /// Not "a chunk parses on its own", which any subset of N-Quads lines does.
    /// The three files in `web/queries/` all start by joining the run's
    /// activity to its `prov:generatedAtTime`, which lives in the HEADER, and
    /// then join a fact in a chunk back onto that same activity. A chunk plus
    /// the header has to satisfy both halves or a truncated run answers nothing
    /// at all, however many quads it holds.
    #[test]
    fn a_chunk_plus_the_header_answers_the_read_queries() {
        let out = header_and_one_chunk();
        let quads = quads_of(&out);

        // One graph, and it is the run's. A chunk in another graph would not
        // join to the header at all, since every read query wraps both in one
        // `GRAPH ?run` block.
        let graphs: BTreeSet<String> = quads.iter().map(|q| q.graph_name.to_string()).collect();
        assert_eq!(
            graphs,
            BTreeSet::from(["<urn:sparqlwatch:run:r1>".to_string()]),
            "header and chunk must be in one graph: {graphs:?}"
        );

        // The join every query makes first: the activity, typed, with its
        // timestamp. Both quads are in the header, and both are needed before
        // any query reaches a measurement.
        let activity = "urn:sparqlwatch:activity:r1";
        assert_eq!(
            object(&quads, activity, RDF_TYPE).map(|t| t.to_string()),
            Some("<http://www.w3.org/ns/prov#Activity>".to_string()),
            "?activity a prov:Activity"
        );
        assert!(
            object(&quads, activity, &format!("{PROV}generatedAtTime")).is_some(),
            "?activity prov:generatedAtTime ?generatedAt"
        );

        // endpoint_measurements.rq, the measured arm: five predicates on one
        // subject, and the last of them joins back to the activity above.
        let measurement = subjects_of(&quads, RDF_TYPE, "<http://www.w3.org/ns/dqv#QualityMeasurement>");
        assert_eq!(measurement.len(), 1, "one measurement in this chunk: {measurement:?}");
        let m = &measurement[0];
        assert_eq!(
            object(&quads, m, &format!("{DQV}computedOn")).map(|t| t.to_string()),
            Some(format!("<{EP}>")),
            "dqv:computedOn reaches the endpoint the query was asked about"
        );
        assert!(object(&quads, m, &format!("{DQV}isMeasurementOf")).is_some());
        assert!(object(&quads, m, &format!("{DQV}value")).is_some());
        assert_eq!(
            object(&quads, m, &format!("{PROV}wasGeneratedBy")).map(|t| t.to_string()),
            Some(format!("<{activity}>")),
            "the measurement joins back to the activity the header typed"
        );

        // The same arm's other half: the not-measured branch of the UNION.
        let declined = subjects_of(&quads, RDF_TYPE, "<urn:sparqlwatch:NotMeasured>");
        assert_eq!(declined.len(), 1, "one declined metric in this chunk");
        let d = &declined[0];
        for p in [
            "urn:sparqlwatch:notMeasuredOn",
            "urn:sparqlwatch:notMeasuredMetric",
            "urn:sparqlwatch:notMeasuredReason",
        ] {
            assert!(object(&quads, d, p).is_some(), "the declined fact carries {p}");
        }
        assert_eq!(
            object(&quads, d, &format!("{PROV}wasGeneratedBy")).map(|t| t.to_string()),
            Some(format!("<{activity}>"))
        );

        // endpoint_content.rq: the sample, joined by `sw:metric:classes` and
        // carrying its size and truncation flag as well as its values.
        let sample = subjects_of(&quads, "urn:sparqlwatch:sampledBy", "<urn:sparqlwatch:metric:classes>");
        assert_eq!(sample.len(), 1, "one class sample in this chunk");
        let s = &sample[0];
        for p in [
            "urn:sparqlwatch:sampledFrom",
            "urn:sparqlwatch:sampleSize",
            "urn:sparqlwatch:sampleTruncated",
            "urn:sparqlwatch:sampledValue",
        ] {
            assert!(object(&quads, s, p).is_some(), "the sample carries {p}");
        }

        // The endpoint itself is typed, so the marker and every `computedOn`
        // name a resource this file describes.
        assert_eq!(
            object(&quads, EP, RDF_TYPE).map(|t| t.to_string()),
            Some("<http://www.w3.org/ns/dcat#DataService>".to_string())
        );

        // And the two facts that say what this file is: the header's terminator
        // present, the footer's absent, which is exactly how `load_run.py`
        // reads an unfinished run.
        assert!(object(&quads, activity, "urn:sparqlwatch:emission").is_some());
        assert!(
            object(&quads, activity, "urn:sparqlwatch:finalised").is_none(),
            "no footer was written, so nothing may claim the run finished"
        );
    }

    /// A sink that answers every write until the nth flush, then fails every
    /// call. Counting flushes rather than writes is what makes "the third
    /// chunk" well defined: `write_endpoint` flushes once per chunk, so the
    /// failure lands at a chunk boundary rather than in the middle of one.
    #[derive(Clone)]
    struct FailsOnChunk {
        wrote: Arc<Mutex<Vec<u8>>>,
        flushes: Arc<Mutex<usize>>,
        fail_at: usize,
    }

    impl Write for FailsOnChunk {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if *self.flushes.lock().unwrap() >= self.fail_at {
                return Err(std::io::Error::other("the disk filled up"));
            }
            self.wrote.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut flushes = self.flushes.lock().unwrap();
            if *flushes >= self.fail_at {
                return Err(std::io::Error::other("the disk filled up"));
            }
            *flushes += 1;
            Ok(())
        }
    }

    /// The unit-level half of the write-failure contract: what was written
    /// before the failure is whole, and nothing of the failing chunk is left
    /// behind as a fragment a reader could take for a finished endpoint.
    ///
    /// The header's flush is the first, so a writer that fails at the third
    /// flush fails on its second chunk.
    #[test]
    fn a_chunk_write_failure_leaves_what_was_written_intact() {
        let run = RunId("r1".into());
        let a = "https://a.example/sparql";
        let b = "https://b.example/sparql";
        let (fa, fb) = (families(a), families(b));
        let sink = FailsOnChunk {
            wrote: Arc::new(Mutex::new(Vec::new())),
            flushes: Arc::new(Mutex::new(0)),
            fail_at: 2,
        };
        let bytes = Arc::clone(&sink.wrote);
        let mut w = RunWriter::with_writer(sink, header(&run)).unwrap();

        w.write_endpoint(facts_for(&run, a, &fa)).expect("the first chunk fits");
        let err = w
            .write_endpoint(facts_for(&run, b, &fb))
            .expect_err("the second chunk must report the failure rather than lose it");
        assert!(err.to_string().contains("disk filled up"), "the cause survives: {err}");

        let written = bytes.lock().unwrap().clone();
        let quads = quads_of(&written);
        let markers: Vec<String> = quads
            .iter()
            .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:completedEndpoint")
            .map(|q| q.object.to_string())
            .collect();
        assert_eq!(
            markers,
            vec![format!("<{a}>")],
            "the endpoint that was written keeps its marker and the one that failed has none"
        );
        assert!(
            !written.windows(b.len()).any(|w| w == b.as_bytes()),
            "no fragment of the failing chunk is on disk"
        );
    }

    /// The checked invariant `emit`'s per-chunk pre-scan rests on. A writer
    /// cannot drop a repeat the way `emit_endpoint` does, because the first
    /// chunk is already on disk, so it refuses and names the endpoint.
    #[test]
    fn a_second_chunk_for_one_endpoint_is_refused_naming_it() {
        let run = RunId("r1".into());
        let all = families(EP);
        let mut w = RunWriter::with_writer(Shared(Arc::new(Mutex::new(Vec::new()))), header(&run))
            .unwrap();
        w.write_endpoint(facts_for(&run, EP, &all)).unwrap();
        let err = w
            .write_endpoint(facts_for(&run, EP, &all))
            .expect_err("a second chunk for one endpoint must be refused");
        assert!(err.to_string().contains(EP), "the refusal names the endpoint: {err}");
    }

    /// The error a constructor refused with. A helper because `RunWriter` does
    /// not implement `Debug` and `expect_err` needs it, and a writer printed on
    /// failure would say nothing a reader wants.
    fn refused(
        result: anyhow::Result<RunWriter<BufWriter<File>>>,
        why: &str,
    ) -> anyhow::Error {
        match result {
            Ok(_) => panic!("{why}"),
            Err(e) => e,
        }
    }

    /// Major 2 of Task 3's review, measured there at 500,000 bytes of a crashed
    /// attempt truncated down to 1,090 by the retry: the `<at>` label exists so
    /// the next run cannot destroy a crashed run's data, and a retry shares the
    /// `--at` by design, so the same loss arrives through the retry unless the
    /// second attempt refuses to open a partial that is already there.
    #[test]
    fn a_retry_sharing_an_at_refuses_rather_than_truncating_the_first_attempt() {
        let dir = std::env::temp_dir().join(format!("sparqlwatch-retry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("run.nq");
        let partial = partial_path(&out, AT);
        let run = RunId(AT.into());
        let all = families(EP);

        // The first attempt, which crashes: its writer is dropped without
        // `finish`, so the partial file stays under its own name.
        let mut first = RunWriter::create(&out, AT, header(&run)).unwrap();
        first.write_endpoint(facts_for(&run, EP, &all)).unwrap();
        drop(first);
        let crashed = std::fs::read(&partial).unwrap();
        assert!(!crashed.is_empty(), "the first attempt left work on disk");

        let err = refused(
            RunWriter::create(&out, AT, header(&run)),
            "a retry must not open the crashed attempt's partial file",
        );
        let message = err.to_string();
        assert!(
            message.contains(partial.to_str().unwrap()),
            "the refusal names the file the operator has to deal with: {message}"
        );
        assert!(
            message.contains("load_run.py") && message.contains("move"),
            "and says what can be done with it, load it or move it aside: {message}"
        );
        assert_eq!(
            std::fs::read(&partial).unwrap(),
            crashed,
            "the crashed attempt's bytes are untouched, which is the whole point"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Minor 1 of the same review: the constructor promises an operator learns
    /// about an unusable `--out` at t=0, and a `--out` that is a DIRECTORY used
    /// to pass that promise and fail in `finish` instead, after the whole sweep.
    #[test]
    fn an_out_that_is_a_directory_is_refused_before_any_probing() {
        let dir = std::env::temp_dir().join(format!("sparqlwatch-outdir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("run.nq");
        std::fs::create_dir_all(&out).unwrap();
        let run = RunId(AT.into());

        let err = refused(
            RunWriter::create(&out, AT, header(&run)),
            "a directory cannot be renamed onto, so the sweep must not start",
        );
        assert!(
            err.to_string().contains(out.to_str().unwrap()),
            "the refusal names --out: {err}"
        );
        assert!(
            !partial_path(&out, AT).exists(),
            "and nothing was created beside it"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A sink whose writes all succeed and whose nth flush, and every flush
    /// after it, fails.
    ///
    /// Separate from `FailsOnChunk` because the line under test is the flush in
    /// `finish` and not the `write_all` before it: a sink that failed the write
    /// too would report an error whether or not `finish` flushes at all, and
    /// the mutation would survive.
    #[derive(Clone)]
    struct FailsOnFlush {
        wrote: Arc<Mutex<Vec<u8>>>,
        flushes: Arc<Mutex<usize>>,
        fail_at: usize,
    }

    impl Write for FailsOnFlush {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.wrote.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut flushes = self.flushes.lock().unwrap();
            *flushes += 1;
            if *flushes >= self.fail_at {
                return Err(std::io::Error::other("the disk filled up"));
            }
            Ok(())
        }
    }

    /// `finish` flushes the footer itself and reports the failure, rather than
    /// leaving the footer to the `Drop` that runs after the rename.
    ///
    /// `BufWriter::drop` flushes and DISCARDS the error, and it runs after the
    /// rename, so without the flush in `finish` an `ENOSPC` at that moment
    /// would return `Ok`, `main` would log the sweep complete and exit zero,
    /// and `--out` would hold a run with no `sw:finalised` in it. Every visitor
    /// would then be told that sweep did not finish.
    ///
    /// The header's flush is the first and the one chunk's is the second, so a
    /// sink that fails at the third fails on the footer's, with the footer's
    /// bytes already accepted by the sink.
    #[test]
    fn a_footer_that_cannot_be_flushed_is_reported_rather_than_left_to_drop() {
        let run = RunId("r1".into());
        let all = families(EP);
        let sink = FailsOnFlush {
            wrote: Arc::new(Mutex::new(Vec::new())),
            flushes: Arc::new(Mutex::new(0)),
            fail_at: 3,
        };
        let bytes = Arc::clone(&sink.wrote);
        let mut w = RunWriter::with_writer(sink, header(&run)).unwrap();
        w.write_endpoint(facts_for(&run, EP, &all)).expect("the chunk's flush is the second");

        let err = w
            .finish(RunFooter { run: &run, failed_endpoints: 0 })
            .expect_err("a footer that cannot be flushed must not report success");
        assert!(err.to_string().contains("disk filled up"), "the cause survives: {err}");
        assert!(
            quads_of(&bytes.lock().unwrap().clone())
                .iter()
                .any(|q| q.predicate.as_str() == "urn:sparqlwatch:finalised"),
            "the sink accepted the footer, so what failed is the flush and not the write"
        );
    }

    /// The invariant `RunWriter`'s owned `run` is there for, made structural.
    ///
    /// A chunk built with another `RunId` lands in a second named graph in the
    /// same file, and `load_run.py` drops and replaces every graph a file
    /// names, so the store would end up with an orphan chunk in a run graph
    /// that has no header. `run_sweep` clones from `writer.run()` so it cannot
    /// happen there; this is what stops a second caller from doing it.
    #[test]
    fn a_chunk_for_another_run_is_refused_naming_both_runs() {
        let run = RunId("r1".into());
        let other = RunId("r2".into());
        let all = families(EP);
        let mut w = RunWriter::with_writer(Shared(Arc::new(Mutex::new(Vec::new()))), header(&run))
            .unwrap();

        let err = w
            .write_endpoint(facts_for(&other, EP, &all))
            .expect_err("a chunk in another run's graph must be refused");
        let message = err.to_string();
        assert!(
            message.contains("r1") && message.contains("r2"),
            "the refusal names the file's run and the chunk's: {message}"
        );

        w.write_endpoint(facts_for(&run, EP, &all))
            .expect("the refusal must not have consumed this endpoint's one chunk");
    }

    /// The name and the rename, together, because they are one guarantee: while
    /// the run is in progress nothing has touched `--out`, and when it finishes
    /// `--out` is this run.
    #[test]
    fn a_run_is_written_to_a_sibling_named_for_it_and_renamed_at_the_end() {
        let dir = std::env::temp_dir().join(format!("sparqlwatch-rename-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("run.nq");
        std::fs::write(&out, b"the previous run\n").unwrap();

        let run = RunId(AT.into());
        let all = families(EP);
        let mut w = RunWriter::create(&out, AT, header(&run)).unwrap();
        w.write_endpoint(facts_for(&run, EP, &all)).unwrap();

        let partial = dir.join(format!("run.nq.{AT}.partial"));
        assert!(partial.exists(), "the run in progress is at {}", partial.display());
        assert_eq!(
            std::fs::read(&out).unwrap(),
            b"the previous run\n",
            "the previous run is untouched while this one is in progress"
        );

        w.finish(RunFooter { run: &run, failed_endpoints: 0 }).unwrap();
        assert!(!partial.exists(), "the partial file is renamed, not copied");
        let finished = std::fs::read(&out).unwrap();
        assert!(
            quads_of(&finished)
                .iter()
                .any(|q| q.predicate.as_str() == "urn:sparqlwatch:finalised"),
            "the finished run is at --out, footer and all"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
