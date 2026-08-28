# Section terminators

Canonical. Two implementations read this file in their test suites, and neither
side may change a spelling here on its own.

A run graph is written as a header of run-level facts, the set of endpoints the
sweep declined to ask, one self-contained chunk per endpoint, and a footer. Each
section ends with its own terminator quad on the run's activity, so a reader
holding a terminator holds the whole section and a reader that does not holds a
fragment it may drop. `prober/src/emit.rs` writes those quads; `web/load_run.py`
cuts a truncated file back to the end of the last one. That makes the four
predicate IRIs a wire format between a Rust writer and a Python reader, with
nothing in either language connecting them: rename one in `emit.rs` and every
partial run cuts back further than it had to, discarding endpoints the crash
preserved, while both suites stay green.

Hence this file. `prober/src/emit.rs`'s tests assert the emitter's four
constants against the table below, and `web/tests/test_load_run.py` asserts the
loader's `_TERMINATOR_PREDICATES` against the same table. A one-sided rename
then reds one of the two suites instead of neither.

The table is read by both tests. It is the fenced block, one section per line,
the section name and then the predicate IRI, in the order a run states them:

```
header urn:sparqlwatch:emission
dormancy urn:sparqlwatch:dormantCount
chunk urn:sparqlwatch:completedEndpoint
footer urn:sparqlwatch:finalised
```

All four are needed, and the loader's own comment says why: a complete run ends
at `finalised`, a run killed between endpoints ends at `completedEndpoint`, a
run killed before its first endpoint ends at `dormantCount`, and a run killed
inside the dormancy section ends at `emission`, so a reader missing any one of
them truncates away a section that was written whole.

The section names are the sections `emit.rs` writes: `header` is what
`emit_header` returns, `dormancy` is what `emit_dormancy` returns, `chunk` is
what `emit_endpoint` returns for one endpoint, `footer` is what `emit_footer`
returns. `write::RunWriter` is what puts the first two out back to back, and it
is the only production writer of a run file. Nothing here says what a
terminator's object is; that is each side's own business, and
`prober/README.md` records it.
