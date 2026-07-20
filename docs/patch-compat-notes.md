# Hermes Agent 0.18.2 `patch` compatibility notes

This note fixes the behavioral reference at Hermes Agent commit
`3f2a389c7e1f1729cad91ae63c26fb08c7753c74`. Later Hermes behavior is not
evidence for this compatibility boundary.

## Conclusion

Hermes `patch` is not an exact string replacement tool. Its published schema
and implementation provide two modes:

- `replace`: targeted replacement using an ordered nine-strategy fuzzy matcher;
- `patch`: V4A multi-file operations supporting update, add, delete, and move.

The Rust runtime now registers `patch` only after implementing both modes. The
replace path uses the ordered nine-strategy matcher with re-indentation,
escape-drift and Unicode guards. The V4A path parses and preflights Update, Add,
Delete, and Move before durable once approval, then rechecks all target
preconditions under sorted Workspace locks before per-file commits. The earlier
exact-only draft is no longer the runtime implementation.

## Pinned upstream evidence

### Schema and dispatch

[`PATCH_SCHEMA`](https://github.com/NousResearch/hermes-agent/blob/3f2a389c7e1f1729cad91ae63c26fb08c7753c74/tools/file_tools.py#L2074)
requires only `mode`, whose enum is `replace | patch`. The mode-specific fields
remain optional in JSON Schema and are checked at dispatch:

- replace requires non-empty `path` and non-null `old_string`/`new_string`;
- V4A requires a truthy `patch` string;
- `replace_all` defaults false;
- upstream also exposes `cross_profile`, which is not appropriate for the
  Profile-bound Rust Workspace authority and should be omitted/rejected locally.

The upstream schema has no `maxLength` and does not set
`additionalProperties: false`. A Rust implementation may be stricter, but its
provider schema must retain both modes and perform the same conditional
validation. Avoid `oneOf` if provider strict-schema portability is uncertain;
keep `mode` required and validate the selected branch in Rust, as upstream does.

### Replace mode

[`patch_replace`](https://github.com/NousResearch/hermes-agent/blob/3f2a389c7e1f1729cad91ae63c26fb08c7753c74/tools/file_operations.py#L1555)
requires an existing readable text file, applies
[`fuzzy_find_and_replace`](https://github.com/NousResearch/hermes-agent/blob/3f2a389c7e1f1729cad91ae63c26fb08c7753c74/tools/fuzzy_match.py#L50),
preserves BOM and line endings, writes through the normal atomic write path,
re-reads to verify persistence, and returns a unified diff plus lint/LSP data.

The first fuzzy strategy that produces candidates wins. The ordered strategies
are exact, line-trimmed, whitespace-normalized, indentation-flexible,
escape-normalized, trimmed-boundary, Unicode-normalized, block-anchor, and
context-aware. A non-`replace_all` call fails if that strategy finds more than
one candidate. Exact matching uses non-overlapping occurrences. Non-exact
replacement also includes re-indentation, escape-drift rejection, guarded tab
and carriage-return unescaping, and Unicode preservation. Block-anchor uses a
0.50 similarity threshold for one candidate and 0.70 for multiple candidates;
context-aware requires at least half the lines to score at least 0.80.

Consequently, exact-only replacement is not a compatible implementation of
Hermes replace mode.

### V4A mode

The pinned
[`patch_parser`](https://github.com/NousResearch/hermes-agent/blob/3f2a389c7e1f1729cad91ae63c26fb08c7753c74/tools/patch_parser.py#L69)
accepts explicit or omitted begin/end markers, lenient file headers, explicit or
implicit context lines, and these operations:

- Update: target must exist; hunks are simulated and then applied in order with
  the same fuzzy matcher. Addition-only hunks append at EOF without a hint or
  insert after one unique hint. A supplied hint that is missing or ambiguous
  fails validation.
- Add: collects `+` lines and calls `write_file`, which creates parents. The
  pinned implementation does not preflight destination existence, so Add can
  overwrite an existing file. A safer local `must not exist` rule is an
  intentional documented divergence, not upstream behavior.
- Delete: target must exist and be readable; deletion is file-only and emits a
  real deletion diff.
- Move: source must exist and destination must not exist; the underlying move
  does not create a missing destination parent.

[`apply_v4a_operations`](https://github.com/NousResearch/hermes-agent/blob/3f2a389c7e1f1729cad91ae63c26fb08c7753c74/tools/patch_parser.py#L344)
prevalidates every operation before writing, so validation failure changes
nothing. Apply is then sequential: a later failure is reported, remaining
operations continue, and earlier changes are not rolled back. It is not a
multi-file transaction. A Rust implementation may provide stronger rollback or
journaling, but must not claim upstream atomicity.

### Limits and security

The pinned patch path has no patch-specific input, operation-count, target-file,
or aggregate-byte limit. `read_file_raw` reads to EOF; the 50 KiB and 2,000-line
constants apply to display reads, not patching. The registry's 100,000-character
value limits tool results, not input or target size.

Upstream still applies important gates: sensitive-path checks at the tool and
file-operation layers, sorted per-path locks, stale-edit warnings, structured
JSON/YAML/TOML validation through `write_file`, and post-write verification.
V4A rejects `..` in every header and checks both move endpoints. Absolute V4A
paths are allowed. Replace paths may contain `..`, and `cross_profile=true` can
override the soft profile guard.

The Rust Workspace contract is intentionally stricter and should stay so:
relative portable paths only, no `..` or absolute paths, no cross-Profile
escape, component-by-component no-follow capability access, sensitive-path
denial, UTF-8 text only, durable once approval, cancellation/deadline polling,
and redacted public events. These are safe local restrictions, not exact
upstream parity.

## Implemented Rust boundary

1. Register `patch` only when both `replace` and V4A `patch` modes are present.
2. Preserve the upstream mode/field names and conditional requirements. Reject
   unknown fields locally and omit `cross_profile` because no such authority is
   granted by a Run Workspace.
3. Port the nine fuzzy strategies and their uniqueness, re-indentation,
   escape-drift, Unicode, and non-overlap behavior. A separate exact helper may
   remain an implementation primitive but is not the Hermes tool.
4. Parse and preflight all V4A operations before side effects. Support Update,
   Add, Delete, and Move, including sequential hunks and addition-only hunks.
5. Retain Rust safety policy: Workspace-relative no-follow paths, sensitive
   exclusions, one durable approval claim, structured-content validation,
   same-directory atomic replacement, permission/BOM/line-ending preservation,
   final read-back verification, and cancellation/deadline checks.
6. Define bounded local policy explicitly because upstream is unbounded. A
   coherent initial policy is 64 KiB raw arguments, 2 MiB per existing target,
   16 MiB aggregate target snapshots, at most 64 operations and 256 hunks, and
   60 KiB internal/provider result output. These numbers are proposed product
   limits, not upstream facts. Truncation must be explicit; never truncate input
   or a candidate file silently.
7. Keep raw patch text, old/new strings, diffs, and file contents inside the
   internal execution journal/provider continuation. Never persist the absolute
   Workspace root in tool payloads. Public approval/tool events expose only
   bounded relative paths, operation counts, and redacted summaries.
8. Return a provider-facing result compatible in meaning with upstream
   `PatchResult`: `success`, bounded `diff`, modified/created/deleted path sets,
   optional replacement/byte counts, and explicit error state.
9. Document multi-file commit semantics. At minimum preserve upstream's
   validate-before-write guarantee and report partial apply explicitly. A
   rollback journal is preferable, but global filesystem atomicity must not be
   claimed without crash/recovery evidence.

## Required test matrix

| Area | Required cases |
| --- | --- |
| Schema | Both modes; branch-required fields; empty versus null strings; `replace_all`; unknown fields; every local size/count limit |
| Exact/ambiguity | One exact match; no match leaves bytes unchanged; ambiguous match fails; `replace_all` uses non-overlapping matches; empty old string and identical old/new fail |
| Fuzzy | One test for each ordered strategy; first-success precedence; 0.50/0.70 block thresholds; 50%-of-lines context rule; fuzzy ambiguity; re-indentation; Unicode preservation; escape-drift rejection; guarded tab/CR handling |
| Text fidelity | UTF-8 only; binary/NUL rejection; BOM and LF/CRLF preservation; permission preservation; structured JSON/YAML/TOML candidate rejected before write; post-write read-back mismatch fails |
| V4A parse | With/without boundary markers; lenient headers; implicit context; malformed Update and Move; empty/context-only patch; multiple operations/hunks |
| V4A update | Sequential hunks see prior results; fuzzy hunk; addition-only with unique hint, no hint, missing hint, and ambiguous hint; files over 2,000 lines and long lines within local byte caps |
| Add/Delete/Move | Add new file and parents; explicitly chosen existing-Add behavior; delete existing/missing/directory; move existing source, missing source, existing destination, missing destination parent, and both path endpoints |
| Preflight/apply | Any validation error writes nothing; injected Nth apply failure verifies the documented partial/rollback behavior; duplicate/conflicting paths, move cycles, and external modification between preflight and commit |
| Workspace security | Absolute path, `..`, backslash/non-portable Windows names, sensitive names, file/dir symlink or reparse-point escape, cross-Profile path, and changed symlink between validation and commit |
| Approval/lifecycle | No side effect before `approval.resolved(once)` and claim CAS; deny, expiry, cancel-first, restart recovery, duplicate decision, claim race, deadline during fuzzy/preflight/staging/commit |
| Journal/privacy | Strict `tool.started -> approval.required -> approval.resolved -> tool.completed|failed`; provider continuation on success/failure; replay after restart; no raw patch, diff, secret, or absolute path in Run, Message, SSE, logs, or public summaries |
