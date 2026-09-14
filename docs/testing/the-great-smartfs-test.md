# The Great SmartFS Test

**Status:** executable plan, not yet executed. Nothing in this document may be marked done on the
basis of a report; only on the basis of a script exit code and its artifacts under `test-results/`.

**Amended 2026-09-13 for [ADR-58](../adr/ADR-58-two-stage-cow-commit.md).** Stage 0 is built
(`crates/smartfs-daemon`, binary `smartfsd`), and ADR-58 then split `cow_commit` into a durable
pending stage plus a background drain. That rewrites the exact code path §2.2 and §5.2 measure, so
both were revised — see the marked blocks. Every revision is recorded as a revision, with what it
used to assert and why it no longer can; none of them weakens a check to make a stage pass, and
§7.3's rule that a script modified to make a stage pass is itself the finding still stands.

**Target:** SmartFS v6.0 (FUSE), Postgres 16 + pgvector, content-addressed blob store on ext4.

**Audience:** the project owner, or an autonomous coding agent (Gemini / Antigravity) with shell
access on the machine that holds `/dev/sda3` and the Docker Postgres container at
`172.17.0.2:5432/smartfs`.

---

## 0. What this is, and what it is not

**What it is.** The Great SmartFS Test is the first time SmartFS is mounted as a real filesystem at
a real path and hit with real POSIX operations by test suites that were not written by this project
and cannot be talked into passing. It is an adversarial, fail-loud verification pass built on four
layers of prior art — pjdfstest for POSIX semantics, xfstests/`generic` for regression coverage, a
transaction-scoped CrashMonkey analog for crash consistency, and the eight Root Invariants as the
oracle every layer is checked against. Its output is a directory of machine-generated evidence:
suite logs, SQL invariant dumps, blob-digest snapshots, and per-stage exit codes.

**What it is not.** It is not another self-graded status report. This project has already been
burned twice by an agent reporting "tests passing" for work that independent reading of the code
showed was never wired in — most recently B-04, where `smartfs-mcp`'s three search tools were
declared fixed to accept plain-text queries while `handler.rs` still reads only `query_vector` and
never calls `smartfs_ai` at all. That false green survived because the tests that were supposed to
catch it open with:

```rust
let pool = match connect_pool(&get_db_url()).await { Ok(p) => p, Err(_) => return };
```

With no reachable Postgres, the test returns having executed **zero assertions**, and `cargo test`
prints it as a pass. That is the single anti-pattern this whole exercise exists to eliminate, so it
is also the hard rule for everything below:

> **The Fail-Loud Rule.** A stage that cannot verify its preconditions — no live database, no live
> mount, no live daemon, no reachable MCP server, missing suite — must exit non-zero. "Skipped"
> is never an outcome. "Couldn't connect" is a FAIL, never a pass, never a warning. Any test that
> can pass without executing an assertion is itself a defect and must be reported as one.

A corollary that matters for reading the results: **Stage 1 is currently expected to FAIL.** That is
not a bug in the test; it is the test doing its job on a known-live defect. See §2.1.

---

## 1. Stage 0 — Build the missing daemon entrypoint (blocking prerequisite)

### 1.1 The gap

`smartfs-fuse` is a **library crate only**. Its `src/` is `error.rs, fs.rs, lib.rs, mount.rs,
state.rs, syntax.rs` — there is no `main.rs` and no `[[bin]]` target in its `Cargo.toml`. It exports
`mount_smartfs()` (blocking) and `spawn_mount_smartfs()` (background thread, returns a
`fuser::BackgroundSession`) from `mount.rs`, on `fuser` 0.15, with `MountOption::DefaultPermissions,
AllowOther, FSName("smartfs"), AutoUnmount`. The only callers of either function anywhere in the
workspace are inside `crates/smartfs-fuse/tests/fuse_integration_tests.rs`.

`smartfs-cli` does not close this gap: it is a one-shot human CLI (`write | cat | history | search |
diff | import | concepts | calibrate | status`), not a daemon, and it has no mount capability.

**Therefore: today, no command exists that produces a live SmartFS mount at a real path.** Every
subsequent stage — pjdfstest, xfstests, crash consistency — needs exactly that and nothing else.
Stage 0 is not optional and cannot be reordered. It is also the one stage that is implementation
work rather than testing work; it is specified here because the test plan is blocked on it, not
because the test plan owns it.

> **DONE 2026-09-13.** `crates/smartfs-daemon` exists and produces `smartfsd`, per §1.2-1.4. Exit
> codes 2-7 were each forced against a live gate rather than read off the source. §1.5 (Stage 0b,
> crash-point instrumentation) is deliberately NOT implemented, so `--crash-points` prints nothing
> and exits 1 and Stage 4 refuses deterministic mode — the loud behaviour §1.5 asks for.
>
> **A second gap of the same kind, not catalogued above:** `smartfs-mcp` is *also* a library crate
> with no `main.rs` and no `[[bin]]` — `src/` is `handler.rs, lib.rs, protocol.rs, server.rs,
> tokens.rs, tools.rs, types.rs`. §2.1's B-04 probe shells out to `target/debug/smartfs-mcp`, which
> therefore cannot exist, and `01_smoke_test.sh` treats its absence as a hard `die`. Stage 1 will
> stop there with "smartfs-mcp binary not found" **before** it can reach a B-04 verdict, so the
> outcome is *unreachable*, not *confirmed*. `McpServer::run_stdio()` already exists in `server.rs`,
> so closing it is a thin `main.rs` — but it is implementation work outside §1 and is recorded here
> rather than done silently.

### 1.2 What to build

Create a new thin binary crate `crates/smartfs-daemon` producing a binary named **`smartfsd`**, and
add it to the workspace `members` list.

Recommended over adding `src/main.rs` + `[[bin]]` to `smartfs-fuse`, for two reasons: it keeps
`smartfs-fuse` a pure library (library consumers do not inherit `clap`, a `#[tokio::main]`, or a
signal handler), and it gives the daemon a place to own the `smartfs-semantic` supervisors without
`smartfs-fuse` gaining a dependency on the consolidation layer — which would put that layer one
import away from the `cow_commit` path and undermine Root Invariant #6 structurally. If the
implementer prefers the `[[bin]]` route anyway, everything below still applies unchanged except the
crate layout.

**Dependencies:** `clap` 4 (derive), `tokio` (`rt-multi-thread`, `macros`, `signal`), `anyhow`,
`tracing` + `tracing-subscriber`, and the workspace crates `smartfs-db`, `smartfs-store`,
`smartfs-fuse`, `smartfs-semantic`. Add `smartfs-ai` only if §1.5 is taken.

**Flags** (defaults deliberately identical to `smartfs-cli`'s, so both tools address the same
system by default):

| Flag | Default | Notes |
|---|---|---|
| `--mountpoint <PATH>` | *(required)* | must exist, be a directory, be empty, not already a mountpoint |
| `--database-url <URL>` | `postgres://postgres:postgres@172.17.0.2:5432/smartfs` | same default as `smartfs-cli` |
| `--store-path <PATH>` | `/var/lib/smartfs/blobs` | same default as `smartfs-cli` |
| `--allow-other` | `false` | adds `MountOption::AllowOther`; xfstests needs it when running as a different uid |
| `--no-semantic` | `false` | skip the consolidation supervisors entirely (used by Stage 2/3 to reduce noise) |
| `--pid-file <PATH>` | none | written after mount succeeds |
| `--ready-file <PATH>` | none | see §1.4 — the scripts poll this, do not make them poll `mountpoint` heuristically |
| `--log-level <LEVEL>` | `info` | `tracing_subscriber::EnvFilter`, also honours `RUST_LOG` |
| `--foreground` | `true` | Stage 4 kills the process directly; never daemonize by default |

### 1.3 Startup sequence — exact order and exact error handling

Every numbered step below is a hard gate: on failure, log at `ERROR` with the underlying error
chain, do **not** proceed, do **not** fall back to a degraded mode, and exit with the listed code.
The one deliberate exception is step 9.

1. Initialize `tracing_subscriber` (before anything that can fail, so failures are visible).
2. Parse args (`clap`).
3. **Validate the mountpoint.** Exists, is a directory, is empty, is not already a mount
   (`/proc/self/mountinfo`). → exit **2**.
4. **Validate the store path.** Exists, is a directory, is writable (create-and-delete a probe
   file). → exit **3**.
5. **Connect the DB pool** via `smartfs-db`'s pool constructor, then run `SELECT 1`.
   → exit **4**. *No `Err(_) => return`. No retry-forever loop that hides an unreachable DB.*
6. **Schema sanity check.** Assert the tables `inode_registry`, `file_versions`, `blobs`,
   `storage_backends` exist and that migrations `001`–`006` are recorded as applied. If anything is
   missing, exit **5** with a message telling the operator to run the migrations. **Never issue
   `CREATE TABLE` here** — Root Invariant #4. (Worth also asserting `blobs` has no `refcount`
   column; its presence means FIX-01 was re-introduced.)
7. **Open the blob store** handle from `smartfs-store` against `--store-path`. → exit **6**.
8. **Build the FUSE state** (`smartfs_fuse::state`) from the pool + store handle, then call
   `spawn_mount_smartfs(state, mountpoint, opts)` with `DefaultPermissions`, `FSName("smartfs")`,
   `AutoUnmount`, plus `AllowOther` when `--allow-other`. Keep the returned
   `fuser::BackgroundSession` alive in a variable for the whole process lifetime — dropping it
   unmounts. → exit **7**.
9. **Start the `smartfs-semantic` consolidation supervisors** unless `--no-semantic`: one task per
   `(plugin_type, model_id)` that has a `consolidation_thresholds` row, each on its own
   `tokio::spawn`, each taking its lock with **`pg_try_advisory_xact_lock`** (transaction-scoped —
   session-scoped locks over a pooled connection is B-02 and is wrong). This is the one non-fatal
   step: if a supervisor fails to start, log `ERROR`, record `semantic=degraded` in the ready file,
   and **keep serving the filesystem**. Root Invariant #6 says this layer never blocks the live
   path; that has to include never blocking startup. Note that with no `consolidation_thresholds`
   row, zero supervisors start by design (fail-safe, not fail-open) — log that count explicitly so
   it is never mistaken for a crash.
10. **Prove the mount is actually serving** before declaring readiness: `stat()` the mountpoint and
    confirm it reports the FUSE filesystem, then write `--pid-file` and `--ready-file`.
11. **Signal handling.** On `SIGINT`/`SIGTERM`: stop the supervisors, drop the `BackgroundSession`
    (unmount), remove the pid/ready files, exit **0**. `SIGKILL` is intentionally unhandled — that
    is Stage 4's instrument.

**Exit codes:** `0` clean, `2` mountpoint, `3` store path, `4` DB connect, `5` schema, `6` store
open, `7` mount failed. Scripts below rely on these being distinguishable.

### 1.4 Why `--ready-file` matters

Stage 2 and Stage 3 will otherwise race the mount: a test suite that starts before FUSE is serving
produces failures that look like filesystem bugs and are not. The ready file must be written only
after step 10 succeeds, and must contain at least the pid, the mountpoint, and `semantic=ok|degraded|off`.
Every script in `scripts/testing/` polls for it and hard-fails on timeout.

### 1.5 Stage 0b — crash-point instrumentation (prerequisite for Stage 4 only)

Stage 4 needs to kill the daemon at *specific* points inside `cow_commit`, not at random. Add to
`smartfs-db` a compile-time-gated macro behind a non-default cargo feature `crash-test`:

- `crash_point!("K7")` expands to nothing unless the `crash-test` feature is on.
- With the feature on, it compares its label against the `SMARTFS_CRASH_POINT` env var and, on a
  match, calls `std::process::abort()` — abort, not `panic!`, so no unwinding, no destructor gets a
  chance to flush or roll back cleanly. That is the point: simulate power loss, not a graceful error.
- `smartfsd --crash-points` prints the list of compiled-in labels and exits 0; without the feature
  it prints nothing and exits 1. Stage 4's script uses this to refuse to run against an
  uninstrumented binary rather than silently degrading to random kills.

Insert the labels at the points named in §5.2. This code must never be in a release build; guard it
in CI with `cargo build --workspace` (no features) and grep the release binary for the label
strings.

---

## 2. Stage 1 — Smoke test: is it alive, and is B-04 real?

**Script:** `scripts/testing/01_smoke_test.sh` · **Runtime:** minutes

Purpose: prove the daemon from Stage 0 actually serves a filesystem, that the two write paths (FUSE
and `smartfs-cli`) converge on the same rows, and that the MCP search path does what it claims.
Nothing here is a performance test; everything here is a liveness-and-truth test.

### 2.1 The B-04 probe — expected to FAIL today

The independent review's B-04 finding, confirmed against `handler.rs`: the dispatch arm for
`search_semantic`, `search_functions` and `search_by_concept` reads only `"query_vector"` from the
JSON-RPC arguments. It never reads `"query"`, and `smartfs_ai` is not called anywhere in
`handler.rs` — even though `tools.rs` advertises a `query` field and
`tests/dispatch_tests.rs::test_search_semantic_and_functions_query_handling` asserts the intended
behaviour (and passes vacuously when Postgres is unreachable).

The probe is a two-call differential, which is what makes it un-fakeable:

1. **Control call.** Take a real embedding vector straight out of the live embeddings table and pass
   it as `query_vector` with no `query`. This *must* return a non-empty result set — a vector is
   always its own nearest neighbour.
2. **Subject call.** Pass the same intent as plain text in `query`, with no `query_vector`.

| Control | Subject | Verdict |
|---|---|---|
| non-empty | non-empty | **PASS** — B-04 is genuinely fixed |
| non-empty | explicit JSON-RPC error | **PASS (honest failure)** — the gap exists but is not silent; record as B-04-partial |
| non-empty | empty list, no error | **FAIL — B-04 CONFIRMED**, the silent-empty-result path is live |
| empty | *anything* | **FAIL — INCONCLUSIVE**, the index or the embedding worker is not populated, so nothing can be concluded; fix that and re-run |

The last row is the important one and is why the script refuses to treat an empty control as a
skip. An inconclusive run is a failed run.

Given the current state of `handler.rs`, the expected outcome on first execution is row 3: **FAIL —
B-04 CONFIRMED.** Whoever runs this should expect a red Stage 1 and should treat a green Stage 1 as
the surprising result requiring explanation.

### 2.2 The rest of Stage 1

> **Revised 2026-09-13 for ADR-58.** `cow_commit` is now two-staged: `release()` returns once the
> blob and a pending marker are durable, and a background drain writes `file_versions` afterwards.
> That splits the checks below into two kinds, and conflating them is how this stage would either
> flake or go falsely green:
>
> - **Checks that must hold immediately, with no waiting at all.** Read-back, `stat` size, and
>   anything else observable through the mount. ADR-58 point 7's read overlay exists precisely so
>   these keep holding across the uncommitted window; a `sleep` before them would *hide* the very
>   regression they are there to catch. If read-back needs a wait, that is a FAIL, not a timing
>   problem to tune away.
> - **Checks that read SQL.** These must first wait for the queue to quiesce — poll
>   `smartfs-cli status` until `Uncommitted markers: 0`, or poll `<store>/pending/queue/` until it
>   is empty, with a hard timeout that FAILs on expiry. Never a fixed `sleep`: a fixed sleep is a
>   guess that turns into a flake, and a flake in this suite is indistinguishable from the bug.
>
> Nothing below was weakened. The version-count and hash assertions are unchanged; only the moment
> at which SQL may be read moved, because the moment the row appears moved.

Against the mount at `/mnt/smartfs-test`:

- create / write / read-back / `stat` a small file; assert bytes round-trip exactly — **immediately,
  with no quiesce first.** This is now also the read-after-write check for ADR-58's overlay
- overwrite it twice, then **wait for quiesce** and assert via SQL that `file_versions` has **three**
  rows for that inode with `version_number` 1,2,3 and three distinct `content_hash` values
  (Root Invariant #2). Contiguous `1,2,3` is still the right assertion: ADR-58 allocates
  `version_number` inside the drain transaction, not at marker time, exactly so numbering stays
  gap-free (ADR-58 §Rozstrzygnięcia #3, rewizja)
- assert that between the write and the quiesce the file **still reads as the new content** — the
  window in which the database is behind must not be observable through the mount
- assert each `content_hash` equals the SHA-256 of the *original* bytes, computed independently in
  the shell (Root Invariant #1) — never trust a hash the daemon reports about itself
- `rename()` within a directory and across directories; `unlink()`; `mkdir`/`rmdir`; a hardlink if
  supported, an explicit documented `ENOTSUP` if not
- write a file whose content is byte-identical to an existing one and assert (after quiesce) that the
  `blobs` table gains **no** new row while `file_versions` gains one (dedup, Invariant #2)
- with the daemon stopped mid-backlog: kill it while markers are queued, restart, and assert every
  acknowledged write reaches `file_versions` without a duplicate. This is the property the pending
  stage exists for, and Stage 1 is where it is cheapest to check
- write through `smartfs-cli write`, read through the FUSE mount, and vice versa; both must be
  visible to the other, with the same `content_hash`
- `smartfs-cli history` / `status` must agree with the SQL, and `status` must report
  `Uncommitted markers: 0` once quiesced
- a truncate-to-zero (`> file`) must not corrupt the version chain

---

## 3. Stage 2 — pjdfstest (POSIX semantics)

**Script:** `scripts/testing/02_run_pjdfstest.sh` · **Runtime:** ~10 min · **Source:**
https://github.com/pjd/pjdfstest

Cheapest real suite and the right first external oracle. It exercises `chmod`, `chown`, `open`,
`link`, `unlink`, `rename`, `mkdir`, `rmdir`, `symlink`, `truncate`, and their error-return edge
cases against POSIX. ZFS-on-Linux used it exactly this way during POSIX-layer development.

Run as root against `/mnt/smartfs-test` (pjdfstest requires root for the `chown` and setuid cases).

Interpretation rules, decided up front so results cannot be rationalized afterwards:

- Every failure is triaged into exactly one of: **(a) a real SmartFS bug**, **(b) an intentional,
  documented SmartFS limitation** (e.g. no xattr support in MVP), or **(c) a FUSE-layer limitation
  outside SmartFS's control**. Category (b) and (c) failures must be recorded in an
  `expected-failures.txt` **with a one-line justification each**, committed to the repo, and
  reviewed. An unjustified entry in that file is itself a finding.
- Baseline the same suite against plain ext4 on `/mnt/smartfs-test`'s backing partition first. Any
  test that fails on plain ext4 is an environment problem, not a SmartFS problem, and must be
  excluded from the comparison — otherwise the numbers are noise.
- The pass rate goes into `test-results/stage2/summary.txt` as raw counts. No prose grade.

---

## 4. Stage 3 — `check-smartfs`: the xfstests `generic/` wrapper

**Script:** `scripts/testing/03_setup_check_smartfs_xfstests.sh` · **Runtime:** hours · **Sources:**
https://github.com/kdave/xfstests, https://github.com/rfjakob/fuse-xfstests

xfstests is the suite ext4, XFS, Btrfs, F2FS, ZFS and bcachefs are all regression-tested with. It
historically had no FUSE support, but there is direct precedent for adapting it: **gocryptfs**
brought it up by writing a `check-gocryptfs` wrapper that mounts the FUSE filesystem where xfstests
expects a native one and then reuses the standard `generic/` group unchanged. Most of `generic/`
passes; tests needing raw block-device features FUSE cannot provide are skipped by the harness
itself. This project's equivalent is **`check-smartfs`**, modelled on that wrapper line for line.

What the wrapper has to do, following the gocryptfs pattern:

1. Set up two loopback-or-partition-backed scratch areas and export the xfstests contract variables:
   `TEST_DEV` / `TEST_DIR` and `SCRATCH_DEV` / `SCRATCH_MNT`. xfstests needs two independent
   filesystems, so SmartFS needs **two daemon instances** — two mountpoints, two store paths, and
   critically **two separate Postgres databases** (`smartfs_test` and `smartfs_scratch`). Sharing one
   database across `TEST_DEV` and `SCRATCH_DEV` will produce inode-namespace collisions that look
   like filesystem corruption.
2. Override the mount/unmount hooks so `_scratch_mount` starts `smartfsd` and waits on its ready
   file, and `_scratch_unmount` sends `SIGTERM` and waits for the unmount, instead of calling
   `mount -t <fstype>`.
3. Override `_scratch_mkfs` to mean: drop and recreate the scratch database from `migrations/`
   (`001`–`006`, in order, as files — Root Invariant #4 forbids improvising DDL here) and empty the
   scratch blob directory. This is the SmartFS analogue of `mkfs`.
4. Declare `FSTYP=fuse.smartfs` and add an exclude list for groups SmartFS cannot support today —
   at minimum `dangerous_*`, `dump`, `quota`, `defrag`, `shutdown`, and anything under the `dax`,
   `realtime` or block-device-injection groups.
5. Run `./check -g generic` and archive `results/` verbatim.

The script this plan ships **scaffolds** the wrapper and marks with explicit `TODO(smartfs)` only
the handful of points that genuinely cannot be inferred without the Stage 0 binary in hand — the
exact daemon invocation, the scratch-DB reset command, and the final exclude list, which can only be
finalized after the first full run produces real failures.

Expect a meaningful set of failures on the first run. The deliverable of Stage 3 is not a green
board; it is a *classified* board: real bug / documented limitation / FUSE-inherent.

---

## 5. Stage 4 — Crash consistency: a transaction-scoped CrashMonkey

**Script:** `scripts/testing/04_crash_consistency_test.sh` · **Runtime:** hours

### 5.1 Why not CrashMonkey itself

CrashMonkey/ACE (HotStorage '17, and the ACM follow-up) is the academic gold standard: it records
block-device write ordering beneath a filesystem and systematically replays every reachable crash
point, then checks the recovered filesystem against a model. That instrument does not fit SmartFS,
because **SmartFS's durability boundary is not the block device — it is the Postgres transaction.**
A block-level replay would be testing Postgres's own crash recovery, which is not the code under
test.

The faithful adaptation keeps the *method* (enumerate crash points systematically, recover, check
invariants) and changes the *boundary*: crash the daemon at every meaningful point around a
`cow_commit`, restart, and assert Root Invariants 1–4 still hold.

### 5.2 The kill points

Derived from the canonical `cow_commit` in `SmartFS_v4.5_to_v5.0_fixes.md` (FIX-04's merged
pseudocode, which supersedes §11.1 KROK 1 and §12.1), plus FIX-02's worker boundary:

> **Revised 2026-09-13 for ADR-58.** The list below was derived from a `cow_commit` that ran KROK 1
> and KROK 2 back to back inside `release()`. ADR-58 moved KROK 2 into a background drain and put a
> durable marker between them, which changes what "correct after a crash" *means* for half these
> points — in one case it inverts it. The revisions are marked per point. The method is untouched:
> enumerate crash points, recover, check invariants.
>
> The single biggest change: **a crash after the marker's `rename()` must RECOVER the write, not
> lose it.** Before ADR-58, a crash inside the transaction meant the write never happened and the
> file correctly read as the previous version. Now the caller has already been told the write
> succeeded, so losing it is data loss, and "the file reads as the previous version" is a FAIL where
> it used to be a PASS. Any expectation below that still reads the old way is a bug in this
> document, not in the daemon.

**KROK 0 — the pending stage (NEW with ADR-58)**

| Label | Kill point | What must be true after restart |
|---|---|---|
| `P1` | after the blob is durable in the store, before the marker is written to `pending/tmp/` | nothing acknowledged, nothing in `pending/queue/`, no `file_versions` row. An orphan blob may exist — invisible through the mount (Invariant #3) and reclaimable by GC-by-scan, identical to `K5` |
| `P2` | after `write`+`fdatasync` into `pending/tmp/`, before the `rename()` into `queue/` | the caller was **not** acknowledged, so nothing may be recovered. A stray file remains under `pending/tmp/`; assert it is never replayed as a write and that `sweep_tmp` reclaims it |
| `P3` | after the `rename()`, before the caller is acknowledged | the marker is durable. POSIX permits the write to be lost from the caller's view here, but SmartFS will recover it anyway: assert that after restart the write **is** committed, exactly once, and that the file reads as the new version |
| `P4` | after acknowledgement, before the drain picks the marker up | **the write must survive.** The caller was told it succeeded. Assert restart commits it exactly once, `content_hash` correct, file reads as the new version, and the marker is gone from `queue/` afterwards |

> **Revised again 2026-09-14 for [ADR-62 phase C](../adr/ADR-62-per-inode-dedup-and-lazy-reclaim.md).**
> Phase C moved dedup off the write path, which inverts `insert_blob` and
> `store.put` — the exact ordering K1-K4 were built around. Those four points
> describe a sequence the code no longer executes, and a crash point that cannot
> fire is not coverage, it is a gap wearing coverage's clothes.
>
> What the write path does now: hash → compress → `store.put` under a
> *provisional* id → marker `rename()` → acknowledge. No database write at all.
> The drain then runs dedup and the version commit in one transaction and
> reconciles the provisional blob.
>
> **The crash surface shrank, and that is the point.** The old order created a
> `blobs` row before the bytes existed, so a crash left a row pointing at
> nothing — FIX-04's "poisoned row", which needed a compensating DELETE and
> FIX-03's existence check to heal. The new order creates the bytes first, so a
> crash leaves an orphan *file* with no row: invisible through the mount by
> Invariant #3, and reclaimed by ADR-62's cleaner. There is no poisoned state to
> compensate for, because the state that needed compensating cannot occur.
>
> | was | now |
> |---|---|
> | `K1` after hash, before `INSERT INTO blobs` | **gone** — no insert here. Covered by `P1` |
> | `K2` after insert, before `store.put` (the poisoned row) | **gone** — this order is inverted. The state is unreachable |
> | `K3` after `store.put`, before `UPDATE compressed_size` | **gone** — the size is known before the row exists and is written with it |
> | `K4` in the `Err` branch, after the compensating `DELETE` | **gone** — nothing to compensate |
> | `K5` end of KROK 1, before `BEGIN` | now `P1`/`P2` below |
>
> They are struck rather than deleted so the record shows what the design used
> to be and why it changed. **New points the drain needs — `D1` and `D2` — are
> listed after KROK 2.**

**KROK 1 — outside any transaction (K1-K4 no longer reachable, see above)**

| Label | Kill point | What must be true after restart |
|---|---|---|
| `K1` | after `SHA-256(data)`, before `INSERT INTO blobs` | nothing written; no new `blobs` row, no new `file_versions` row |
| `K2` | after `INSERT INTO blobs … RETURNING`, before `store.put` (`inserted=TRUE`) | a `blobs` row exists with **no physical blob** and **no** `file_versions` referencing it. This is exactly FIX-04's poisoned row. Assert no `file_versions` row references it, then re-write the same content and assert the FIX-03 `store.exists` heal (or the FIX-04 compensating `DELETE`) makes the content readable |
| `K3` | after `store.put` Ok, before `UPDATE blobs SET compressed_size` | blob present on disk, `compressed_size IS NULL`. Must be tolerated, not treated as corruption; content must still read back correctly |
| `K4` | inside the `Err` branch, after the compensating `DELETE FROM blobs`, before returning `Err` | no `blobs` row, no orphan blob file, next write of the same content succeeds cleanly |
| `K5` | end of KROK 1, before `BEGIN` | blob exists on disk and in `blobs`, but **zero** `file_versions` reference it. It must be invisible through the mount (Invariant #3) and reclaimable by GC-by-scan. *Under ADR-58 this is `P1`: KROK 1 is followed by the marker write, not by `BEGIN`.* |

**KROK 2 — inside the transaction (now executed by the drain, not by `release()`)**

> Every point below crashes a transaction whose marker is already durable. So "full rollback" is
> still the right assertion **for that transaction**, and it is no longer the end of the story: the
> marker survives, the startup scan replays it, and the write must end up committed. Each row states
> both halves. Asserting only the rollback half would let a lost acknowledged write pass as correct.

| Label | Kill point | What must be true after restart |
|---|---|---|
| `K6` | after `SELECT … FOR UPDATE` and the `MAX(version_number)+1` computation, before `INSERT INTO file_versions` | transaction rolls back fully; **then** the replay commits the write exactly once and the file reads as the **new** version. Reading as the previous version after replay has finished is a FAIL |
| `K7` | after `INSERT INTO file_versions`, before `INSERT INTO ast_nodes` | full rollback, no half-version; then replay commits it exactly once, with its AST nodes |
| `K8` | after `INSERT INTO ast_nodes`, before `UPDATE inode_registry` | full rollback, `inode_registry.current_blob_id` still on the previous blob; then replay moves it to the new one |
| `K9` | after `UPDATE inode_registry`, immediately before `COMMIT` | full rollback — still the highest-value point, since any surviving row means the "short transaction, SQL only, zero I/O" property of KROK 2 is not real. Then replay commits it exactly once |

**KROK 2b — the drain's own transaction (NEW with ADR-62 phase C)**

| Label | Kill point | What must be true after restart |
|---|---|---|
| `D1` | after `insert_blob` inside the drain transaction, before `cow_commit` in the same transaction | full rollback of **both**: no `blobs` row, no `file_versions` row. The marker survives, so replay must then commit the write exactly once. A surviving `blobs` row here would mean the two statements are not actually one transaction, which is the whole point of phase C |
| `D2` | after the drain's `COMMIT`, before the provisional blob is reconciled | the version is committed and correct. A *duplicate* write leaves its redundant provisional blob on disk: assert it is unreferenced, therefore invisible (Invariant #3), and reclaimed by the cleaner rather than leaking. This is the deferred-dedup cost made observable |

**Post-commit and concurrency**

| Label | Kill point | What must be true after restart |
|---|---|---|
| `K10` | *(pre-ADR-58 wording: immediately after `COMMIT`, before the FUSE reply reaches the caller)* — **the FUSE reply now happens long before the COMMIT, so this point no longer exists as written.** Its successor is: immediately after `COMMIT`, before the marker is unlinked | the version is committed and the marker is still on disk — exactly the state the idempotency key exists for. Assert the replay is a **no-op**: exactly one row per `(inode_id, version_number)`, no second version, `content_hash` correct, file reads as the new version, and the marker is gone afterwards. The pre-ADR-58 caller-visible case is now `P3` |
| `K11` | during a concurrent `smartfs-ai` worker cycle, mid `finish_embed` / `refresh_is_current` | **at most one** `is_current=TRUE` per inode's AST node set (FIX-02); no version left stuck in `processing` forever — restart must reclaim it to `pending`; and the worker's activity must not have left a `file_versions` row mutated |
| `K12` | kill **Postgres** (`docker kill`) rather than the daemon, at the K9 and K10 equivalents | same assertions as K9/K10; additionally the daemon must exit non-zero or recover cleanly, never serve stale reads from a dead pool |
| `K13` | `pg_terminate_backend()` on the daemon's connection mid-transaction | transaction rolls back; the daemon surfaces an error to the caller rather than reporting success |

**Back-pressure and the queue (NEW with ADR-58)**

| Label | Kill point | What must be true after restart |
|---|---|---|
| `Q1` | kill the daemon while the pending queue holds a deep backlog | every acknowledged write in the queue is committed exactly once after restart, in `seq` order, with `version_number` contiguous. The seeded gate must refuse new writes with `EAGAIN` until the backlog drains, never accept them into an unbounded queue |
| `Q2` | stop Postgres, write until the queue is full, restart Postgres | writes are refused with `EAGAIN` while it is down (never silently accepted and dropped), and every write that *was* acknowledged commits once the drain recovers. `EAGAIN` on a refused write is the correct outcome; a successful `write()` whose data never lands is not |

Plus a **stochastic mode**: a hammer loop of concurrent writes with `kill -9` at random intervals,
run for N iterations. This catches points nobody thought to label. It complements, and does not
replace, the deterministic list — a random-only crash test is how a project convinces itself it is
crash-safe without ever hitting the one window that matters.

### 5.3 The assertion list (Root Invariants 1–4)

Run after **every** crash-and-restart round, against a scratch database, never the main one:

**Invariant #1 — `content_hash` is SHA-256 of the original bytes, before compression.**
For every `file_versions` row with `blob_id IS NOT NULL AND external_path IS NULL`: locate the blob
file, decompress it, compute SHA-256 independently in the shell, and compare to `content_hash`.
Also assert `size` equals the decompressed byte length and `content_hash` matches `^[0-9a-f]{64}$`.
A hash computed *after* compression will show up here as a total mismatch, not a subtle one.

**Invariant #2 — every content change is a new `file_versions` row; blobs are never mutated.**
Snapshot `sha256sum` of every file under `--store-path` before the round and again after. **Any
pre-existing blob file whose digest changed is an outright Invariant #2 violation** — content-
addressed storage is append-only by definition. Additionally assert `version_number` values per
inode are unique and contiguous `1..n`, that no two versions of an inode share a `version_number`,
and that `parent_version_id` forms a single unbroken chain.

**Invariant #3 — the daemon is the only legal path to data.**
For every regular file visible through the mount, SHA-256 its bytes and assert the digest equals the
`content_hash` of the newest `file_versions` row for its inode — **after quiescing the pending
queue.** Before quiesce the newest content legitimately has no row yet: it is served from ADR-58's
read overlay and its marker is in `pending/queue/`. Checking without quiescing would report correct
behaviour as a violation. Checking that a file's bytes match *neither* its newest row *nor* any
queued marker remains a violation at any time. No visible file may lack a backing
`file_versions` row. Conversely, count blob files on disk with no referencing `file_versions` row:
those are GC candidates and are reported as a count, not an automatic failure — but a *growing*
count across rounds is a leak and is reported as one.

**Invariant #4 — migrations are explicit SQL files; no runtime DDL.**
Snapshot `information_schema.tables` + `information_schema.columns` for the schema before the whole
stage and diff after. **Any difference at all is a failure** — it means something issued DDL at
runtime. Also assert `blobs` has no `refcount` column (its reappearance means FIX-01 was
re-introduced) and that the applied-migration list is exactly `001`–`006`.

Every round writes its raw SQL output and digest snapshots to
`test-results/stage4/<label>/<iteration>/` so a disputed result can be re-examined instead of
re-argued.

---

## 6. Stage 5 — Where this methodology is headed

Stages 2–4 are all, ultimately, *sampling*: pjdfstest samples POSIX corner cases someone thought to
write down, xfstests samples regressions someone else's filesystem once hit, and the crash stage
samples the crash points listed in §5.2. None of them explores the state space systematically, so
none of them can tell you what they *didn't* look at.

The current research frontier closes that gap. **Metis** and **Themis** (2024–2026, see
arXiv:2608.01135) are model-checking harnesses for filesystems: rather than fuzzing randomly, they
enumerate reachable states of the filesystem under a model and check each against a reference
implementation, which is how they surface bugs that decades of xfstests runs did not.

This is named here as direction, **not as work to schedule now**. Two things have to be true before
it is even sensible to attempt: SmartFS needs a stable daemon entrypoint (Stage 0) and a passing
`generic/` baseline (Stage 3), and it needs a reference model to check against — which, for a
filesystem whose durability boundary is a SQL transaction, is a genuine research question rather
than a configuration exercise. The reason to write it down now is the project's stated long-term
ambition of becoming a kernel-native root filesystem. Nothing gets to be a root filesystem on the
strength of a sampled test suite; the eventual bar is a model-checked one, and knowing that now
should shape how the invariant oracles in §5.3 are written — as machine-checkable predicates over
database state, which is exactly the form a model checker will later need.

---

## 7. How to run this

Addressed directly to whoever executes this — human or agent. Follow the order. Do not skip a
prerequisite because it "looks fine". Do not report a stage as passing on the basis of anything
other than a zero exit code from its script.

### 7.1 Before anything

Run `scripts/testing/00_preflight_checks.sh`. It exits non-zero on any failure and prints exactly
what failed. It verifies:

1. **The scratch partition.** `/dev/sda3` exists, is `ext4`, has the filesystem label
   `smartfs-test`, and is mounted at `/mnt/smartfs-test-backing`. **Critically, it verifies this
   partition is not the machine's real data pool** — it re-reads the label and refuses to continue
   if it does not match, and it refuses if the mountpoint resolves onto the same device as `/` or
   `/home`. This test destroys data on that partition. If the check is ambiguous, it fails; it never
   guesses.
2. **Postgres.** The container at `172.17.0.2:5432/smartfs` accepts a connection, `SELECT 1`
   succeeds, `pgvector` is installed, and migrations `001`–`006` are applied. Unreachable is a
   **FAIL**, never a skip.
3. **The build.** `cargo build --workspace` succeeds. Then, specifically, it checks whether a
   `smartfsd` binary exists — and if not, it fails with the Stage 0 message, because that is the
   real blocker and it should be stated in those words rather than discovered three scripts later.

### 7.2 Then, in order

```
scripts/testing/00_preflight_checks.sh            # must exit 0 before anything else
#   → if it reports "STAGE 0 NOT DONE", implement §1 first. Nothing below can run.
scripts/testing/01_smoke_test.sh                  # expect FAIL on the B-04 probe today (§2.1)
scripts/testing/02_run_pjdfstest.sh
scripts/testing/03_setup_check_smartfs_xfstests.sh   # scaffolds; then run ./check-smartfs -g generic
scripts/testing/04_crash_consistency_test.sh      # scratch DB only; never the main database
```

### 7.3 Rules for reporting results

- Attach the contents of `test-results/` . A claim without an artifact is not a result.
- State counts, not grades: "pjdfstest 8231/8412, 181 failures, 174 in expected-failures.txt, 7 new".
- Every new failure gets triaged into real-bug / documented-limitation / FUSE-inherent, with the
  justification written down at the time, not reconstructed later.
- If a stage could not run, say **which precondition was missing** and stop. Do not proceed to the
  next stage and do not describe the run as partially successful.
- If any script is modified to make a stage pass, that modification is itself the finding and must
  be reported before the result is.
