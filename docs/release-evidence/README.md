# Mixed Runtime Release Evidence

This directory defines the machine-checkable evidence boundary for the eight-hour
mixed-runtime soak. It intentionally contains no candidate report, manifest,
approval, reviewer identity, or example that could be mistaken for a release
claim.

Run the verifier self-test with the repository-pinned Node version:

```powershell
node scripts/verify-mixed-runtime-evidence.mjs --self-test
```

The self-test builds synthetic fixtures only in memory. It does not write an
evidence file or approve a candidate.

## Evidence Pair

A real review requires two JSON inputs:

- The raw schema-v2 output from `scripts/verify-mixed-runtime.mjs`.
- A schema-v1 candidate manifest written after a human has reviewed the RSS
  trend.

Both files should be retained as release artifacts. The raw report path recorded
by the manifest must be a normalized path below `docs/release-evidence/`. The
manifest also names the exact backend binary used by the soak. Both JSON inputs
must use canonical single-line `JSON.stringify(value) + "\n"` bytes; reformatted
or otherwise rewritten JSON is rejected even when it parses to the same value.
All digests use `sha256:` followed by 64 lowercase hexadecimal characters.

The manifest has these exact top-level fields:

- `schemaVersion`: `1`.
- `kind`: `synthchat-mixed-runtime-candidate-evidence`.
- `report`: normalized `path`, exact byte count, and SHA-256 of the raw bytes.
- `candidate`: Git commit, platform, architecture, Node/Rust versions, effective
  configuration SHA-256, and the platform's exact release backend path and
  SHA-256.
- `command`: executable plus the exact argument vector used for the soak.
- `rssReview`: reviewer, canonical review timestamp, `accepted` decision,
  summary, sample accounting, first/last/peak RSS, and full/final-window slopes.

The canonical argument vector records the eight-hour duration, concurrency,
cycle delay, failure limit, reported latency retention limit, Provider delay,
resource interval/capacity, tool-probe interval, candidate backend path,
`--skip-build`, and raw output path. The verifier rejects reordered, omitted,
duplicated, or extra arguments.

## Build And Run

Use Node 22.14.0 in a fresh clean candidate checkout. The report and manifest
paths must not exist when the producer starts. Building under `backend/target/`
does not dirty the source checkout because Cargo outputs are ignored.

On Windows x64, build and run exactly as follows. Keep forward slashes in the
Node argument vector because the producer records the argv bytes exactly:

```powershell
cargo +1.88.0-x86_64-pc-windows-msvc build --locked --release `
  --manifest-path backend/Cargo.toml --bin synthchat-hermes-backend
node scripts/verify-mixed-runtime.mjs `
  --duration-seconds 28800 --concurrency 2 --cycle-delay-ms 3000 `
  --max-failures 25 --latency-sample-limit 5000 --provider-delay-ms 10 `
  --resource-interval-ms 5000 --resource-sample-limit 5762 --resource-samples `
  --tool-every-iterations 10 `
  --backend-bin backend/target/release/synthchat-hermes-backend.exe --skip-build `
  --output docs/release-evidence/mixed-runtime-8h.json
```

On Linux or macOS, use the same canonical arguments with the Unix binary path:

```bash
cargo +1.88.0 build --locked --release \
  --manifest-path backend/Cargo.toml --bin synthchat-hermes-backend
node scripts/verify-mixed-runtime.mjs \
  --duration-seconds 28800 --concurrency 2 --cycle-delay-ms 3000 \
  --max-failures 25 --latency-sample-limit 5000 --provider-delay-ms 10 \
  --resource-interval-ms 5000 --resource-sample-limit 5762 --resource-samples \
  --tool-every-iterations 10 \
  --backend-bin backend/target/release/synthchat-hermes-backend --skip-build \
  --output docs/release-evidence/mixed-runtime-8h.json
```

The verifier rejects debug, custom, or renamed backend paths. Windows must use
`backend/target/release/synthchat-hermes-backend.exe`; Linux and macOS must use
`backend/target/release/synthchat-hermes-backend`.

## Producer Provenance

Raw schema v2 contains a strict `provenance` object captured after resolving the
backend binary and before starting it. Its nested `schemaVersion` is `1` and it
records:

- Platform, architecture, Node version, and Rust toolchain.
- Git object format, commit, tree, and whether the producer worktree was clean.
- Producer script and backend binary paths, exact byte counts, and SHA-256
  digests.
- SHA-256 digests of the sorted effective configuration and exact producer argv.
- Names of non-empty `SYNTHCHAT_MIXED_VERIFY_*` overrides and a digest of their
  sorted name/value entries.

Release evidence requires `overrideNames` to be empty and its digest to equal the
canonical digest of `[]`. The manifest's effective-configuration digest must
match the producer value. The argv digest is independently recomputed from
`manifest.command.arguments.slice(1)`, which corresponds to the producer's
`process.argv.slice(2)`.

Validate a completed pair from the candidate checkout and on the same target
host that produced it:

```powershell
node scripts/verify-mixed-runtime-evidence.mjs `
  --report docs/release-evidence/mixed-runtime-8h.json `
  --manifest docs/release-evidence/mixed-runtime-8h.manifest.json
```

The command fails closed unless the raw report proves all of the following:

- An exact 28,800,000 ms configured pilot completed successfully.
- Every started iteration completed successfully and all failure records are
  empty.
- Periodic `session_search` probes, Provider continuations, and public Run/SSE
  events satisfy their conservation equations.
- Every latency family accounts for its configured retention capacity without
  failures.
- Shutdown was graceful and backend, Provider, and temporary data were cleaned.
- Resource samples span the full window, contain no dropped samples, keep both
  skipped and unavailable RSS probes at or below one percent, account for every
  unavailable backend RSS value, remain inside the report/workload timeline,
  retain available RSS near both endpoints, and reproduce both slopes from
  densely sampled windows.
- The report hash matches its exact bytes and the current producer script and
  backend binary match the producer-time byte counts and hashes.
- The backend path is the exact platform release path; debug, custom, and
  renamed binaries are rejected.
- Candidate commit/tree/object format and runtime fields match both the raw
  provenance and current verifier context; the producer worktree was clean.
- The exact command matches the raw configuration, its argv digest matches the
  producer value, no producer override was active, and the human RSS decision is
  `accepted`.

The verifier also requires the current candidate checkout and index to be clean.
The report and manifest may be the only untracked paths when they live inside the
workspace; every tracked modification, staged change, or unrelated untracked
path fails the check.

## Boundary

Producer-time provenance closes the raw-v1 lineage gap for the executed script,
backend bytes, Git tree, runtime, argv, and effective configuration digest. It
does not prove that the named backend binary was built from that Git commit, and
the verifier cannot independently reconstruct every internal effective-config
field from the report. Candidate builds must retain their separate reproducible
build, signing, and artifact attestations.

The packaged Desktop backend sidecar requires a separate hash attestation. After
packaging, record the SHA-256 of the backend executable extracted from the
candidate package and prove that its bytes match the soaked release backend's
`provenance.backend.sha256` (renaming the sidecar does not change its bytes). This
verifier does not inspect a package or establish that package-to-soak equality,
so a passing evidence pair cannot replace that attestation.

The verifier recomputes RSS metrics and checks the manifest values. It does not
authenticate the human reviewer, define an acceptable leak or latency threshold,
replace platform signing or package tests, or turn a synthetic self-test into
release evidence. Those decisions remain separate release gates.

Historical schema-v1 raw reports predate producer-time provenance and the strict
tool-probe, Provider-counter, event-conservation, and RSS-unavailable fields.
They are rejected rather than upgraded or interpreted as release evidence.
