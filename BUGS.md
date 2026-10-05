# bacup code review — bug report

Full-repo review of all 18 files under `src/` (2026-10-04), re-audited
2026-10-05 against the post-fix tree (commits 3fec877, 17c8d90, a631331,
09e12ff, ca34f22). Findings ordered by severity: data-corruption / wrong-data
bugs first, then panic hazards, then functional oddities. Items marked
[FIXED] were verified against the current tree and the fix batch.

**Second pass found a latent cleanup hole in #1.8 (the `cleanup()` local-file
deletion branch is dead code). Note: this is *not* an active disk leak —
normal runs delete the local dump via `Dump::drop`, verified on gtr7.**

## 1. Data-corruption / wrong-data bugs

### 1.1 [FIXED] `src/services/postgresql.rs` `dump()` — pg_dump exit status ignored

The `match` on `.status().await` returns `Ok(Dump)` for *any* successful spawn,
discarding the `ExitStatus`. A non-zero `pg_dump` exit (permissions, lock wait,
OOM) is reported as a successful backup and the empty/truncated `.sql` gets
uploaded. **Fix:** check `status.code()` and return an error on non-zero.

### 1.2 `src/remotes/git.rs` `upload_file` — file nested one level too deep

`dest = repo.join(remote_path)` is the *file* path; the code does
`create_dir_all(dest)` then copies to `dest.join(file_name)`, so the first
upload creates `repo/<subdir>/mydb.sql/mydb.sql`. Every later run reuses the
directory, so the wrong nesting is stable.
**Fix:** `create_dir_all(dest.parent()?)` + `copy(path, dest)`.

### 1.3 `src/remotes/git.rs` `upload_folder` — directory tree flattened

Every file is copied to `dest.join(file_name)` regardless of its subdirectory,
so nested structures are lost and name collisions are silently overwritten.
**Fix:** preserve relative paths, like the localhost/aws remotes do.

### 1.4 `src/remotes/ssh.rs` `upload_folder` — only one directory synced when the glob matches several

`local_prefix = paths.min().parent()`. With a pattern like `/home/u/*/data`
(or any glob spanning sibling directories) rsync only sends the
lexicographically-first directory; the rest are silently skipped while
`--delete` keeps the remote "consistent" with that subset.
**Fix:** resolve the common ancestor of all matched paths (or require a single
root and error otherwise).

### 1.5 [FIXED] `src/remotes/ssh.rs` `enumerate` / `delete` — unquoted remote paths

`find {}/*` and `rm -r {}` interpolate `remote_path` raw, while `upload_file`
(correctly) uses the `shell_quote` helper. A path with spaces breaks `find` and
makes `rm -r` target the wrong thing — `delete` is the most dangerous one.
**Fix:** use the existing `shell_quote` helper in both commands.

### 1.6 `src/remotes/ssh.rs` `upload_file` — no atomic upload; partial files on failure

`cat > file` writes directly to the timestamped name. A network drop or
remote-disk-full mid-transfer leaves a truncated "backup" that later runs
treat as valid. **Fix:** upload to `name.tmp` and `mv` into place on success
(same for the compressed variant).

### 1.7 [FIXED] `src/remotes/aws.rs` `enumerate` — `list_objects_v2` not paginated

Only the first 1000 keys are ever seen, so `keep_last` pruning silently stops
working beyond 1000 objects per prefix.
**Fix:** loop on `continuation_token` / `is_truncated`.

### 1.8 [FIXED] `src/services/zfs.rs` `cleanup()` — local-file deletion branch was dead code

`do_dump` calls `self.cleanup(&snapshots, &new_name)` where `new_name` is the
ZFS **snapshot** name (`dataset@base-full-<ts>`, no `.snapshot` suffix), but
`cleanup` → `old_dump_files` → `parse_snapshot_name` does
`strip_suffix(".snapshot")?` on that argument, which fails, so
`old_dump_files` always returns `vec![]` and the `fs::remove_file` loop never
runs. The test `old_dump_files_are_strictly_older` passes a *file* name as
`newer_name`, masking the mismatch.

**This is *not* an active disk leak.** The primary cleanup path is
`Dump::drop` (`src/services/service.rs`), which removes the dump file when
the `Dump` value goes out of scope at the end of the scheduled job in
`backup.rs`. Verified in production on gtr7: the current
`/storage/zroot-full-20261005-100000.snapshot` was already gone from the
working directory after the upload, and no older `*.snapshot` files
accumulated. `cleanup()`'s file deletion is only a redundant second layer.

The real (narrow) leak window: if the process crashes (panic / SIGKILL /
OOM) between `zfs send` completing and `Drop` running, the raw stream is
left in the working directory and nothing will ever remove it — full dumps
are raw `zfs send` streams, so one stale file can be large.

**Fix:** just delete the dead code. `Dump::drop` is the real cleanup path
and it works (verified in production), so the file-deletion layer in
`cleanup()` — the `fs::read_dir` + `old_dump_files` + `fs::remove_file`
loop — plus `old_dump_files` itself and its test
`old_dump_files_are_strictly_older` should be removed. Keep the rest of
`cleanup()` (`cleanup_list` + `destroy`), which is the live ZFS snapshot
cleanup. Optionally note in `Drop`'s doc that a crash between `send` and
drop can leave a stale file behind.

**Applied:** removed the `fs::read_dir` / `old_dump_files` / `fs::remove_file`
block from `cleanup()`, deleted `old_dump_files` and its test, updated the
`cleanup()` doc to point at `Dump::drop`, and documented the crash-orphan
caveat in `Drop`'s doc in `service.rs`. Full test suite passes (48 + 6 ok).

## 2. Panic hazards (crash instead of `Err`)

### 2.1 `status.code().unwrap()` on signal-killed processes

`docker.rs` (`new` + `dump`), `postgresql.rs` `new`, `ssh.rs` `new` (error
path). A SIGKILLed docker/pg_isready/ssh panics the whole scheduler instead of
returning an error. `status.code()` is `None` for signal-killed processes.

### 2.2 [FIXED] `String::from_utf8(...).unwrap()` on command output

`ssh.rs` `new`, `postgresql.rs` `new` (both stdout and stderr). Non-UTF8
output (e.g. a broken locale message) panics startup.

**Applied:** command output is best-effort diagnostic text (locale-dependent
remote banners/messages), so both `ssh.rs` and `postgresql.rs` now decode it
with `String::from_utf8_lossy` (the postgresql half landed as a drive-by in
#3.7, the ssh half directly). This is the only place lossy is *required* by
the strict-vs-lossy rule — the strict-text sites (`config.rs` TOML load,
private key load) already return `Err` on non-UTF8 via `fs::read_to_string`,
and the aws binary path keeps `Vec<u8>` with no text assumption.

### 2.3 `.unwrap()` on collections that can be empty / non-UTF8

- `aws.rs` + `ssh.rs` `upload_folder`: `paths.iter().min_by(...).unwrap()` (empty paths)
- `remote.rs` `compress_folder`: `path.file_name().unwrap()` (root-level path)
- `git.rs`: `remote_path.strip_prefix("/").unwrap()` (empty remote_path)
- `remote.rs` / `postgresql.rs`: `std::env::current_dir().unwrap()`

### 2.4 `src/bin/bacup.rs`

Every service/remote constructor call ends in `.unwrap()` (including
`Backup::new(...).await.unwrap()`), so one misconfigured service kills the
entire process with a backtrace instead of the clean `error!` +
`return Err(-1)` style used by the keep_last gate.

### 2.5 [FIXED] `src/backup.rs` scheduled job — `.unwrap()` on empty file list

`local_files.iter().min_by(...).unwrap()` in the cron job panicked when
`service.list()` returned nothing (e.g. a folder glob that matches no files).
`Backup::new` uses `if let Some(...)`, so only the scheduled path was exposed.
Fixed with an early `error!("...no files, nothing to back up") + return` guard
right after `service.list()` in the job.

## 3. Functional oddities (lower severity)

### 3.1 [FIXED] `src/remotes/git.rs` `clone_repository` — clone dir is `<CWD>/<last-segment-of-repo>`

Two git remotes whose repos share a basename silently share the first repo's
clone (the `dest.exists()` shortcut). Also, a failed `git pull` (diverging
shallow-clone branch) leaves conflict markers in the worktree, and
`git add . -A` commits them as a "backup" — pull failure output is ignored.

### 3.2 [FIXED] `src/remotes/git.rs` `upload_file_compressed` — deferred cleanup targets the wrong path

The deferred `fs::remove_file(&remote_path)` operates on the *remote* path
string (e.g. `/backups/x.sql-YYYY-MM-DD-HH.MM.gz`) as if it were a local path:
silently no-ops, `#[must_use]` result ignored. It should remove the local temp
file under the repo dir.

### 3.3 [FIXED] `src/remotes/git.rs` push-failure error message

Says "Unable to execute git add . -A" — copy-paste from the add step; the
failing command is `git push`.

### 3.4 [FIXED] `src/remotes/ssh.rs` encrypted-key detection

Matches only legacy PEM (`Proc-Type` + `ENCRYPTED`). Modern OpenSSH-format
encrypted keys are not flagged, so users get a confusing auth failure instead
of the "key is encrypted" hint.

Fix: added `openssh_key_is_encrypted()` in src/remotes/ssh.rs — base64-decodes
the `-----BEGIN OPENSSH PRIVATE KEY-----` blob, validates the `openssh-key-v1`
magic, and checks the cipher name field against `"none"`; `Ssh::new` now rejects
passphrase-protected OpenSSH-format keys with the same `InvalidPrivateKey`
error as the legacy path. Added the `base64` crate (0.22.1) dependency.
4 new unit tests (unencrypted/encrypted OpenSSH keys, legacy PEM out of scope,
malformed blocks).

### 3.5 [FIXED] `src/remotes/remote.rs` archive names have minute precision

`%H.%M` means two folder/file backups finishing within the same minute produce
the same remote name; the second overwrites the first. (zfs dumps avoid this:
one file per run with second precision.)

### 3.6 [FIXED] `src/services/docker.rs` `new` — connectivity check pulls an image

Runs `docker run --rm hello-world` on every startup, downloading the
hello-world image each time and requiring network.

Fix: `Docker::new` now runs `docker info` instead — validates the daemon is
running and reachable with no image pull and no network.

### 3.7 [FIXED] `src/services/postgresql.rs` misc

- db-existence check builds SQL by interpolation (`datname='{db_name}'`), so a
  quote in the configured name breaks/mangles the query (low severity: config
  is user-managed).
- The psql connection check treats *any* non-empty stderr as failure, so
  benign locale warnings fail startup.
- No password field in config: `pg_dump` runs with `--no-password`, so
  authentication relies entirely on `~/.pgpass`.

Fix:
- single quotes in `db_name` are SQL-doubled (`'` → two single quotes) before
  the name is interpolated into the `datname` query.
- The check now fails on `!status.success()` (exit status), not on any
  non-empty stderr; stdout is expected to be exactly `"1"`.
- `PostgreSqlConfig` (and the `PostgreSql` service) gained
  `password: Option<String>` (serde `#[default]`, existing configs unaffected);
  when set, both `psql` and `pg_dump` get it via the `PGPASSWORD` env var.
- psql stdout/stderr now read with `String::from_utf8_lossy` instead of
  `str::from_utf8`. (6 existing test literals updated with `password: None`.)

### 3.8 [FIXED] `src/remotes/aws.rs` `put_object` memory spike

Buffers whole files ≤ 1024 MiB entirely into a `Vec` before upload — a 1 GB
dump spikes RSS by 1 GB unnecessarily. A streaming body would fix it.

Fix: `put_object` no longer reads the file into a `Vec` — the small-file path
now uses `ByteStream::from_path(path)`, which streams the file from disk and
sets `Content-Length` from file metadata (aws-smithy-types 1.8.1 has no
`ByteStream::from(tokio::fs::File)`; the earlier note described a planned
approach, not what shipped). `CHUNK_SIZE` now only decides put-vs-multipart;
the unused `tokio::fs::File` and `AsyncReadExt` imports are gone.

### 3.9 [FIXED] `src/bin/bacup.rs` main loop — tick errors invisible

The `scheduler.tick()` error check is commented out and replaced with a bare
`sleep(50ms)` loop, so scheduler tick failures are never surfaced.

### 3.10 [FIXED] Typos in user-visible messages / comments

- `remote.rs`: "Storagee error" (should be "Storage")
- `ssh.rs`: "succeded"
- `postgresql.rs`: "does not exit" (→ "exist"); comments: "shuld be
  performend", "aksing"

### 3.11 [RESOLVED BY #3.5] `src/remotes/remote.rs` `remote_compressed_file_path` — space in timestamp

Re-checked after the 3.5 fix: both `remote_archive_path` and
`remote_compressed_file_path` now build names with
`now.format("%Y-%m-%d-%H.%M.%S")` (no space, seconds precision) — the
`ts.replace(...)` code no longer exists. No action needed.

### 3.12 [FIXED] `src/when.rs` `parse_weekly` — `.contains()` day matching is a substring scan

`day.to_lowercase().contains(day_name)` matches substrings, not whole words
(no two day names are substrings of each other, and the HH:MM regex rejects
the leftovers, so no practical misfire — but `"tuesday 10:00"` containing
`"tue"`-style input is not a supported format anyway). Cosmetic; note only.

Fix: `parse_weekly` now matches day names as whole words (whitespace-token
membership) and removes only the matched word when consuming the input,
instead of `.contains()`/`.replace()` substring scans. All existing when
tests still pass unchanged.

## 4. What's solid

- `zfs.rs` full+incremental chain logic, keep_last gate, delegated-permission
  check, compression nudge, and the `when`/cron `full_when` resolution all
  check out (the dead local-file cleanup in `cleanup()` was removed — see
  #1.8; the real cleanup is `Dump::drop`).
- `when.rs` (shared when-format module, fixed dow mapping + regression test),
  `folders.rs`, `localhost.rs`, `service.rs`, `config.rs`, `lib.rs`
- The keep_last startup gate, `what`/`where` validation, and the zfs
  `required_keep_last` upper-bound simulation are correct (conservative by ≤ 1
  when `full_when` fires between backup runs — safe direction).

## Suggested fix order (second pass, 2026-10-05)

1. ~~#1.8 remove the dead local-file cleanup code in zfs `cleanup()`~~ [DONE]
2. #2.1–2.4 panic sweep (all are `unwrap()` on signal/utf8/empty inputs)
3. #1.2 / #1.3 git nesting + flattening
4. #1.4 ssh multi-dir glob, #1.6 ssh atomic upload
5. ~~#3.4, #3.6, #3.7, #3.8, #3.12~~ [DONE — applied and verified with `cargo build` + `cargo test` (58 passed, 2 ignored)]
