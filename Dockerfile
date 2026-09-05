# One image that serves sparqlwatch, and carries the prober binaries so a sweep
# can be run in the same environment that serves.
#
# TWO STAGES, and the first one is temporary in more than the usual sense.
# docs/superpowers/specs/2026-08-31-one-webapp-design.md moves the prober into
# Python; when that lands, the Rust stage below is deleted and the runtime stage
# stays as it is. The layout here is chosen so that removal is a deletion rather
# than a rewrite.

# ---------------------------------------------------------------------------
# Stage 1: build the three Rust binaries.
#
# prober/Cargo.toml declares rust-version = "1.96", so the tag is pinned to it
# rather than to `latest`: a toolchain floating ahead of the declared floor is a
# build that can break without a commit.
# ---------------------------------------------------------------------------
FROM rust:1.96-slim-bookworm AS prober-build

WORKDIR /src

# Manifests first, so a change to source does not invalidate the dependency
# layer. There is no cargo-chef here on purpose: this stage exists to be deleted,
# and the extra machinery would have to be deleted with it.
COPY prober/Cargo.toml prober/Cargo.lock ./prober/

# reqwest needs a TLS stack. Installing ca-certificates in the BUILDER is not
# enough for the runtime stage, which installs its own: a binary that can make a
# TLS handshake but cannot verify a certificate fails on every https endpoint,
# which is most of the registry.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

COPY prober/src ./prober/src
RUN cd prober && cargo build --release --locked \
 && strip target/release/sparqlwatch-prober \
          target/release/dormancy \
          target/release/seed-registry

# ---------------------------------------------------------------------------
# Stage 2: the runtime.
# ---------------------------------------------------------------------------
FROM python:3.12-slim-bookworm AS runtime

# ca-certificates because the prober speaks TLS to strangers. curl for the
# healthcheck below and for nothing else.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# Dependencies before application code, so editing a template does not reinstall
# pyoxigraph. requirements.txt pins every version exactly, which is what makes
# this layer reproducible rather than merely cached.
COPY web/requirements.txt /app/web/requirements.txt
RUN pip install --no-cache-dir -r /app/web/requirements.txt

# The application. web/ holds the modules, the queries and the templates, and
# pytest.ini and tests/ come with it so the suite can be run inside the image.
#
# What that run proves and what it does not. 324 of the 342 tests exercise the
# runtime: the readers, the loader, the pages, the negotiation. The other 18 check
# that the SITE'S COPY of a fact still matches the PROBER'S SOURCE, reading .rs
# files and docs/. A runtime image does not ship the sources it was built from, so
# those 18 SKIP here, by a guard in web/tests/conftest.py, and CI checks them
# against the repo before an image is built. Expect "324 passed, 18 skipped"; a
# FAILURE there is a real finding.
COPY web/ /app/web/

# What the prober reads: its metric definitions, the endpoint registry, and the
# exclusion list. The exclusion list is re-read every run by design, so it is a
# file in the image rather than something baked into a binary.
COPY prober/metrics.toml /app/prober/metrics.toml
COPY prober/endpoints.toml /app/prober/endpoints.toml
COPY prober/registry/ /app/prober/registry/
# The synthetic endpoint, so the compose file can run it as the local control
# without a checkout. It needs nothing the web tier has not already installed:
# both are pyoxigraph over the same Python.
COPY tools/synthetic/ /app/tools/synthetic/

COPY --from=prober-build /src/prober/target/release/sparqlwatch-prober /usr/local/bin/
COPY --from=prober-build /src/prober/target/release/dormancy /usr/local/bin/
COPY --from=prober-build /src/prober/target/release/seed-registry /usr/local/bin/

# TWO VOLUMES, and they are not interchangeable.
#
#   /data/store   the Oxigraph store. DERIVED and rebuildable from the runs, so
#                 losing it costs a reload and nothing else.
#   /data/runs    the run files. THE SOURCE OF TRUTH. A sweep observes a
#                 changing world, so a lost run is not reproducible, it is gone.
#                 This is the one that needs a backup.
#   /data/state   the dormancy state. Small, and the prober fails closed without
#                 it, so it is mounted rather than ephemeral.
#
# Declared so that running without -v is loud in `docker inspect` rather than
# silently writing the source of truth into a layer that vanishes on rm.
RUN mkdir -p /data/store /data/runs /data/state
VOLUME ["/data/store", "/data/runs", "/data/state"]

# Non-root, and it must own /data or the first write fails. The uid is fixed so a
# host bind mount's ownership can be matched deliberately.
RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin sparqlwatch \
 && chown -R sparqlwatch:sparqlwatch /data /app
USER sparqlwatch

WORKDIR /app/web
ENV SPARQLWATCH_STORE=/data/store/sparqlwatch.db \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

EXPOSE 8000

# ONE WORKER, and this is an invariant rather than a default.
#
# The Oxigraph store admits exactly one writer. web/README.md:94 states it, and
# it was measured on 2026-08-31: a second read-write open is refused with an
# IO error on the LOCK file, while a read-only open succeeds. Once the merged
# prober writes from the serving process, a second worker reintroduces the very
# lock problem the merge exists to remove.
#
# So --workers is spelled out here instead of omitted. An omitted flag defaults
# to one today and is the sort of thing a later "let us scale it" commit changes
# without knowing what it is breaking.
#
# THE CONTAINER DOES NOT SWEEP ON STARTUP, and must not. A sweep sends requests
# to several hundred third-party endpoints; a container that swept when it
# started would probe all of them again on every restart, crash loop included.
# Sweeping is an explicit act:
#
#   docker exec <c> sparqlwatch-prober --at 2026-09-01T00:00:00Z \
#       --endpoints /app/prober/endpoints.toml \
#       --metrics /app/prober/metrics.toml \
#       --state /data/state/dormancy.toml \
#       --out /data/runs/run-2026-09-01T00-00-00Z.nq
CMD ["python", "-m", "uvicorn", "app:app", \
     "--host", "0.0.0.0", "--port", "8000", "--workers", "1"]

# Hits a real route rather than a synthetic /healthz, so a pass means the store
# opened and a page rendered. start-period is generous because the first request
# is what opens the store.
HEALTHCHECK --interval=30s --timeout=10s --start-period=40s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8000/about > /dev/null
