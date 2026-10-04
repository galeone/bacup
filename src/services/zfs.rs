// Copyright 2022-2026 Paolo Galeone <nessuno@nerdz.eu>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use croner::Cron;
use log::{debug, info, warn};
use tokio::fs;
use tokio::process::Command;

use crate::config::ZfsConfig;
use crate::services::service::{Dump, Service};
use crate::when;

const TS_FORMAT: &str = "%Y%m%d-%H%M%S";
const FULL_MARK: &str = "full";
const INC_MARK: &str = "inc";
const SNAPSHOT_EXT: &str = ".snapshot";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotKind {
    /// A full snapshot, taken when the `full_when` schedule was due.
    Full,
    /// An incremental snapshot taken against a previous snapshot.
    Incremental,
    /// A snapshot taken by a bacup version without full/incremental naming.
    /// Legacy snapshots count as valid bases for incrementals and are
    /// cleaned up like any other of our snapshots.
    Legacy,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    /// The full name as reported by `zfs list`, e.g. `tank@snap-full-20260101-010000`.
    /// Child datasets carry the same snapshot name (e.g. `tank/data@snap-full-...`).
    pub name: String,
    pub kind: SnapshotKind,
    /// Time encoded in the snapshot name.
    pub time: DateTime<Utc>,
}

/// Parse a `zfs list` snapshot name (e.g. `tank/data@snap-inc-20260101-010000`)
/// into a [Snapshot] if it belongs to the snapshot chain rooted at
/// `snapshot_base`, i.e. it is named `<base>-full-<ts>`, `<base>-inc-<ts>`
/// (new style) or `<base>-<ts>` (legacy style). Returns `None` for names that
/// do not belong to this chain.
pub(crate) fn parse_snapshot(name: &str, snapshot_base: &str) -> Option<Snapshot> {
    let after_at = name.split('@').nth(1)?;
    let rest = after_at.strip_prefix(snapshot_base)?.strip_prefix('-')?;
    let (kind, ts) = if let Some(ts) = rest.strip_prefix(&format!("{FULL_MARK}-")) {
        (SnapshotKind::Full, ts)
    } else if let Some(ts) = rest.strip_prefix(&format!("{INC_MARK}-")) {
        (SnapshotKind::Incremental, ts)
    } else {
        (SnapshotKind::Legacy, rest)
    };
    let naive = NaiveDateTime::parse_from_str(ts, TS_FORMAT).ok()?;
    let time = DateTime::from_naive_utc_and_offset(naive, Utc);
    Some(Snapshot {
        name: name.to_string(),
        kind,
        time,
    })
}

/// Format the timestamp part used in snapshot names and dump file names.
pub(crate) fn format_ts(now: DateTime<Utc>) -> String {
    now.format(TS_FORMAT).to_string()
}

/// Local dump file name for a new snapshot, e.g. `mysnap-inc-20260101-010000.snapshot`.
/// Remote names track local names, so the name must be unique per snapshot
/// for the incremental chain to survive on the remote side.
pub(crate) fn dump_file_name(name: &str, kind: SnapshotKind, ts: &str) -> String {
    let mark = match kind {
        SnapshotKind::Full => FULL_MARK,
        SnapshotKind::Incremental => INC_MARK,
        // Legacy snapshots are never created; their dump files use the
        // legacy file name `<name>.snapshot` directly.
        SnapshotKind::Legacy => return format!("{name}{SNAPSHOT_EXT}"),
    };
    format!("{name}-{mark}-{ts}{SNAPSHOT_EXT}")
}

/// Arguments for the `zfs send` command to dump the given snapshot.
/// A full snapshot is sent with `send -R -v -c -L`, an incremental one with
/// `send -R -v -c -L -i <base>` where `<base>` is the name of the snapshot it
/// is based on.
///
/// `-c` keeps blocks compressed in the stream when they are compressed on
/// disk (OpenZFS >= 2.1.1), and `-L` keeps the on-disk block sizes so that
/// `-c` is not defeated by the `large_blocks` feature (without `-L`, data is
/// decompressed before sending to be split into smaller blocks). For datasets
/// without compression the two flags are a no-op. Dump files of compressed
/// datasets are therefore already compressed and do not need the gzip layer
/// at upload time (`compress = false`).
pub(crate) fn send_args(kind: SnapshotKind, base: Option<&str>, new_name: &str) -> Vec<String> {
    match (kind, base) {
        (SnapshotKind::Incremental, Some(base)) => {
            vec![
                "send".into(),
                "-R".into(),
                "-v".into(),
                "-c".into(),
                "-L".into(),
                "-i".into(),
                base.into(),
                new_name.into(),
            ]
        }
        // A full snapshot, or (defensively) an incremental without a base.
        _ => vec![
            "send".into(),
            "-R".into(),
            "-v".into(),
            "-c".into(),
            "-L".into(),
            new_name.into(),
        ],
    }
}

/// Decide whether the next run at `now` should take a full or an
/// incremental snapshot.
///
/// A full is due when `full_when` is absent (always full, previous behavior),
/// when no full snapshot exists yet, or when the next occurrence of
/// `full_when` after the last full is not in the future. Otherwise the run
/// is an incremental against the latest existing snapshot of any kind. If no
/// base snapshot exists (e.g. first run) an incremental is impossible, so a
/// full is taken.
pub(crate) fn select_plan(
    now: DateTime<Utc>,
    full_when: Option<&Cron>,
    snapshots: &[Snapshot],
) -> Vec<String> {
    let is_full_due = match (
        full_when,
        latest_of_kind(snapshots, Some(SnapshotKind::Full)),
    ) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(cron), Some(last_full)) => match cron.find_next_occurrence(&last_full.time, false) {
            Ok(next) => next <= now,
            // On parse issues fall back to a full: safe and self-healing.
            Err(_) => true,
        },
    };

    if is_full_due {
        return vec!["full".to_string()];
    }

    match latest_of_kind(snapshots, None).map(|s| s.name.clone()) {
        Some(base) => vec!["inc".to_string(), base],
        // No base snapshot available, so an incremental is impossible.
        None => vec!["full".to_string()],
    }
}

/// Returns the latest snapshot in `snapshots` of the given kind, or the
/// latest snapshot of any kind when `kind` is `None`.
fn latest_of_kind(snapshots: &[Snapshot], kind: Option<SnapshotKind>) -> Option<&Snapshot> {
    snapshots
        .iter()
        .filter(|s| kind.map(|k| s.kind == k).unwrap_or(true))
        .max_by(|a, b| a.time.cmp(&b.time))
}

/// The names of the snapshots to destroy after a new full snapshot has been
/// created and sent: every snapshot of our chain except the new full
/// (which, being the newest, is the only one the incremental chain needs).
pub(crate) fn cleanup_list(new_full_name: &str, snapshots: &[Snapshot]) -> Vec<String> {
    snapshots
        .iter()
        .map(|s| s.name.clone())
        .filter(|n| n != new_full_name)
        .collect()
}

/// Filter out the new-style local dump files (matching `<name>-{full,inc}-<ts>.snapshot`)
/// older than the snapshot with name `newer_name`, i.e. the ones to remove
/// after a new full backup. Entries that do not belong to our chain are dropped.
pub(crate) fn old_dump_files(entries: &[String], name: &str, newer_name: &str) -> Vec<String> {
    let Some(newer) = parse_snapshot_name(newer_name, name) else {
        return vec![];
    };
    entries
        .iter()
        .filter_map(|e| parse_snapshot_name(e, name))
        .filter(|s| s.time < newer.time)
        .map(|s| s.name.clone())
        .collect()
}

#[derive(Debug)]
pub enum Error {
    ZfsError(std::io::Error),
    String(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ZfsError(e) => write!(f, "zfs error: {e}"),
            Error::String(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::ZfsError(e)
    }
}

impl From<String> for Error {
    fn from(e: String) -> Self {
        Error::String(e)
    }
}

/// Zfs service, based on the zfs command line tool.
///
/// With `full_when` set, runs where the expression is due take a full backup
/// and the runs in between take incrementals against the latest existing
/// snapshot of the chain; without it, every run is a full backup.
#[derive(Clone)]
pub struct Zfs {
    name: String,
    cmd: String,
    dataset: String,
    snapshot_base: String,
    full_when: Option<Cron>,
}

/// Parses a schedule expression, accepting the human friendly formats of
/// the `when` field (e.g. "monthly 1 01:00") and falling back to a raw
/// cron expression.
pub fn parse_schedule(expr: &str) -> Result<Cron, Error> {
    let parsable = when::parse_when(expr).unwrap_or_else(|_| expr.to_string());
    Cron::from_str(&parsable).map_err(|e| {
        Error::String(format!(
            "invalid schedule expression {expr}: not a valid when format nor a cron expression ({e})"
        ))
    })
}

/// Returns the minimum `keep_last` a zfs backup needs so the remote always
/// retains a restorable chain: the latest full snapshot plus every
/// incremental taken since it.
///
/// An incremental stream (`zfs send -i base`) cannot be restored without its
/// base snapshot, so `keep_last` must survive the longest stretch of
/// incremental runs between two consecutive fulls. `backup_when` is the
/// backup's `when` field, `full_when` the zfs service's `full_when`; both
/// accept the human when formats or raw cron expressions.
///
/// Returns an error if the expressions cannot be parsed or if `full_when`
/// fires less than twice in the four year simulation window, in which case
/// the required `keep_last` cannot be bounded.
pub fn required_keep_last(backup_when: &str, full_when: &str) -> Result<u32, Error> {
    let backup = parse_schedule(backup_when)?;
    let full = parse_schedule(full_when)?;

    // A fixed four year window (spanning a leap year) is enough to observe
    // every gap pattern the two periodic schedules can produce. The fixed
    // start keeps the result deterministic.
    let from = NaiveDate::from_ymd_opt(2026, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .expect("fixed simulation window start is a valid date");
    let until = from + chrono::Duration::days(4 * 365 + 1);

    let backup_runs = occurrences(&backup, from, until)?;
    let full_runs = occurrences(&full, from, until)?;
    if full_runs.len() < 2 {
        return Err(Error::String(format!(
            "full_when '{full_when}' fires less than twice in four years, \
             the required keep_last cannot be bounded"
        )));
    }

    // Max number of backup runs strictly between two consecutive fulls.
    let mut max_inc = 0usize;
    for pair in full_runs.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        // Strictly between: a backup at lo or hi is that full's own run.
        let before = backup_runs.partition_point(|t| *t <= lo);
        let up_to = backup_runs.partition_point(|t| *t < hi);
        max_inc = max_inc.max(up_to - before);
    }

    // One for the full snapshot itself, one per incremental in the longest
    // gap.
    Ok(max_inc as u32 + 1)
}

/// All occurrences of `cron` in `[from, until]`.
fn occurrences(
    cron: &Cron,
    from: NaiveDateTime,
    until: NaiveDateTime,
) -> Result<Vec<NaiveDateTime>, Error> {
    let mut out = Vec::new();
    if cron
        .is_time_matching(&from)
        .map_err(|e| Error::String(e.to_string()))?
    {
        out.push(from);
    }
    let mut cursor = from;
    while out.len() < 100_000 {
        let next = cron.find_next_occurrence(&cursor, false).map_err(|e| {
            Error::String(format!(
                "schedule '{}' stopped producing occurrences: {e}",
                cron.as_str()
            ))
        })?;
        if next > until {
            break;
        }
        out.push(next);
        cursor = next;
    }
    Ok(out)
}

impl Zfs {
    pub async fn new(config: &ZfsConfig, name: &str) -> Result<Zfs, Error> {
        let dataset = &config.dataset;
        let snapshot_base = &config.snapshot_name;

        let zfs =
            which::which("zfs").map_err(|_| Error::String("zfs not found in PATH".to_string()))?;
        let cmd = zfs.to_str().unwrap().to_string();

        let full_when = match &config.full_when {
            Some(expr) => Some(parse_schedule(expr)?),
            None => None,
        };

        // Check that the current user is allowed to manage snapshots of the dataset.
        let user = std::env::var("USER").map_err(|_| {
            Error::String(
                "USER environment variable is not set, cannot check zfs permissions".to_string(),
            )
        })?;
        let allow_output = Command::new(&cmd)
            .arg("allow")
            .arg(dataset)
            .output()
            .await?;
        if !allow_output.status.success() {
            return Err(Error::String(format!(
                "failed to check zfs permissions on {dataset}: {}",
                String::from_utf8_lossy(&allow_output.stderr)
            )));
        }
        let allow_text = String::from_utf8_lossy(&allow_output.stdout).to_string();
        let required = ["destroy", "mount", "send", "snapshot"];
        let ok = allow_text.lines().any(|line| {
            let line = line.trim();
            let is_user_line = line.starts_with(&format!("user {user}"))
                || line.starts_with(&format!("user '{user}'"))
                || line.starts_with(&format!("user \"{}\"", user));
            is_user_line && required.iter().all(|p| line.contains(p))
        });
        if !ok {
            let required_list = required.join(",");
            return Err(Error::String(format!(
                "user \"{user}\" is not allowed to manage zfs snapshots on {dataset}. \
                 Run `zfs allow {user} {required_list} {dataset}` (permissions are inherited \
                 by child datasets)."
            )));
        }

        // The dump is produced with `zfs send -c -L` (see `send_args`): blocks
        // compressed on disk stay compressed in the stream. Gzip'ing such a
        // dump at upload time is wasted CPU with no size gain, so flag it when
        // the dataset is compressed.
        let compress_output = Command::new(&cmd)
            .args(["get", "-p", "-o", "value", "compression", dataset])
            .output()
            .await?;
        if !compress_output.status.success() {
            return Err(Error::String(format!(
                "failed to query compression on {dataset}: {}",
                String::from_utf8_lossy(&compress_output.stderr)
            )));
        }
        let compression = String::from_utf8_lossy(&compress_output.stdout)
            .trim()
            .to_string();
        if compression != "off" {
            info!(
                "dataset {dataset} is compressed ({compression}): the zfs dump is already compressed, consider compress = false for its backups"
            );
        }

        debug!("new zfs service on {dataset} (snapshot base {snapshot_base})");
        Ok(Zfs {
            name: name.to_string(),
            cmd,
            dataset: dataset.clone(),
            snapshot_base: snapshot_base.clone(),
            full_when,
        })
    }

    /// List all snapshots of our chain on the dataset tree, including those
    /// on child datasets (created with `-r`).
    async fn list_our_snapshots(&self) -> Vec<Snapshot> {
        let output = match Command::new(&self.cmd)
            .arg("list")
            .arg("-r")
            .arg("-H")
            .arg("-o")
            .arg("name")
            .arg("-t")
            .arg("snapshot")
            .arg(&self.dataset)
            .output()
            .await
        {
            Ok(output) if output.status.success() => output,
            _ => return vec![],
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter_map(|line| parse_snapshot(&line, &self.snapshot_base))
            .collect()
    }

    async fn create_snapshot(&self, name: &str) -> Result<(), Error> {
        let status = Command::new(&self.cmd)
            .arg("snapshot")
            .arg("-r")
            .arg(name)
            .status()
            .await?;
        if !status.success() {
            return Err(Error::String(format!(
                "zfs snapshot {name} failed (exit {status})"
            )));
        }
        Ok(())
    }

    async fn send(&self, args: &[String], dest: &str) -> Result<(), Error> {
        let dest_file = fs::File::create(dest).await?;
        let status = Command::new(&self.cmd)
            .args(args)
            .stdout(Stdio::from(dest_file.try_into_std().unwrap()))
            .status()
            .await?;

        if !status.success() {
            return Err(Error::String(format!("zfs send failed (exit {status})")));
        }
        Ok(())
    }

    async fn destroy(&self, name: &str) -> bool {
        match Command::new(&self.cmd)
            .arg("destroy")
            .arg(name)
            .status()
            .await
        {
            Ok(status) => status.success(),
            Err(e) => {
                warn!("failed to run zfs destroy {name}: {e}");
                false
            }
        }
    }
}

#[async_trait::async_trait]
impl Service for Zfs {
    async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
        self.do_dump()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    async fn list(&self) -> Vec<PathBuf> {
        self.list_files().await
    }
}

impl Zfs {
    async fn do_dump(&self) -> Result<Dump, Error> {
        let now = Utc::now();
        let ts = format_ts(now);

        let snapshots = self.list_our_snapshots().await;
        let plan = select_plan(now, self.full_when.as_ref(), &snapshots);
        let is_full = plan[0] == "full";
        let kind: SnapshotKind = if is_full {
            SnapshotKind::Full
        } else {
            SnapshotKind::Incremental
        };
        let base = plan.get(1).cloned();
        let new_name = format!(
            "{}@{}-{}-{}",
            self.dataset,
            self.snapshot_base,
            if is_full { FULL_MARK } else { INC_MARK },
            ts
        );
        let dest = dump_file_name(&self.name, kind, &ts);

        info!(
            "taking {} zfs snapshot {} of {}{}",
            if is_full { "full" } else { "incremental" },
            new_name,
            self.dataset,
            base.as_ref()
                .map(|b| format!(" based on {b}"))
                .unwrap_or_default()
        );
        self.create_snapshot(&new_name).await?;

        if is_full {
            // A full chain replaces all previous ones: dump it and clean up.
            let send = send_args(kind, base.as_deref(), &new_name);
            self.send(&send, &dest).await?;
            info!("ZFS full checkpoint completed successfully");
            self.cleanup(&snapshots, &new_name).await;
        } else {
            // Incremental: fall back to a full if the base is gone
            // (e.g. a child dataset was destroyed and recreated).
            let base = base.expect("plan guarantees a base for incrementals");
            let send = send_args(kind, Some(&base), &new_name);
            if self.send(&send, &dest).await.is_err() {
                warn!(
                    "incremental send from base {base} failed; \
                     destroying the incremental snapshot and retrying as a full backup"
                );
                if self.destroy(&new_name).await {
                    info!("incremental snapshot {new_name} destroyed");
                } else {
                    warn!("failed to destroy incremental snapshot {new_name}");
                }
                let full_name = format!("{}@{}-{FULL_MARK}-{ts}", self.dataset, self.snapshot_base);
                self.create_snapshot(&full_name).await?;
                let send = send_args(SnapshotKind::Full, None, &full_name);
                self.send(&send, &dest).await?;
                info!("ZFS full checkpoint completed successfully (incremental fallback)");
                self.cleanup(&snapshots, &full_name).await;
            } else {
                info!("ZFS incremental checkpoint completed successfully (base {base})");
            }
        }

        Ok(Dump {
            path: Some(dest.into()),
        })
    }

    /// List the dump files this service should upload: the newest new-style
    /// dump file (or the legacy `<name>.snapshot` if none exists yet).
    async fn list_files(&self) -> Vec<PathBuf> {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut entries = vec![];
        let mut newest: Option<Snapshot> = None;

        let mut read_dir = match fs::read_dir(&current_dir).await {
            Ok(rd) => rd,
            Err(e) => {
                warn!("could not read dump directory {:?}: {e}", current_dir);
                return entries;
            }
        };
        loop {
            let Some(entry) = (match read_dir.next_entry().await {
                Ok(Some(e)) => Some(e),
                Ok(None) => None,
                Err(e) => {
                    warn!("could not read dump directory entry: {e}");
                    None
                }
            }) else {
                break;
            };
            let file_name = entry.file_name().to_string_lossy().to_string();
            // New-style dump files: <name>-{full,inc}-<ts>.snapshot
            let Some(snap) = parse_snapshot_name(&file_name, &self.name) else {
                continue;
            };
            if newest.as_ref().map(|n| snap.time > n.time).unwrap_or(true) {
                newest = Some(snap);
            }
        }

        if let Some(snap) = newest {
            entries.push(current_dir.join(snap.name));
        } else if let Ok(legacy) = fs::metadata(format!("{}{SNAPSHOT_EXT}", self.name)).await {
            if legacy.is_file() {
                entries.push(current_dir.join(format!("{}{SNAPSHOT_EXT}", self.name)));
            }
        }

        entries
    }
}

/// Parse a local dump file name into a [Snapshot] if it belongs to the chain
/// rooted at `name` (new-style `<name>-{full,inc}-<ts>.snapshot`).
/// The returned [Snapshot::name] is the file name itself.
fn parse_snapshot_name(file_name: &str, name: &str) -> Option<Snapshot> {
    let stem = file_name.strip_suffix(SNAPSHOT_EXT)?;
    let after_name = stem.strip_prefix(name)?.strip_prefix('-')?;
    // Local dump files are only ever new-style.
    let (kind, ts) = if let Some(ts) = after_name.strip_prefix(&format!("{FULL_MARK}-")) {
        (SnapshotKind::Full, ts)
    } else {
        let ts = after_name.strip_prefix(&format!("{INC_MARK}-"))?;
        (SnapshotKind::Incremental, ts)
    };
    let naive = NaiveDateTime::parse_from_str(ts, TS_FORMAT).ok()?;
    let time = DateTime::from_naive_utc_and_offset(naive, Utc);
    Some(Snapshot {
        name: file_name.to_string(),
        kind,
        time,
    })
}

impl Zfs {
    /// After a successful full: destroy all older snapshots of our chain on
    /// the dataset tree and remove old local dump files. Best effort: failures
    /// are logged but do not fail the backup.
    async fn cleanup(&self, snapshots: &[Snapshot], new_full_name: &str) {
        let to_destroy = cleanup_list(new_full_name, snapshots);
        let mut destroyed = 0;
        for name in &to_destroy {
            if self.destroy(name).await {
                info!("destroyed old zfs snapshot {name}");
                destroyed += 1;
            } else {
                warn!("failed to destroy old zfs snapshot {name}");
            }
        }
        if !to_destroy.is_empty() {
            info!(
                "destroyed {destroyed}/{} old zfs snapshot(s)",
                to_destroy.len()
            );
        }

        let files: Vec<String> = match fs::read_dir(".").await {
            Ok(mut read_dir) => {
                let mut files = vec![];
                while let Ok(Some(entry)) = read_dir.next_entry().await {
                    files.push(entry.file_name().to_string_lossy().to_string());
                }
                files
            }
            Err(e) => {
                warn!("failed to list local dump files for cleanup: {e}");
                vec![]
            }
        };
        for file in old_dump_files(&files, &self.name, new_full_name) {
            match fs::remove_file(&file).await {
                Ok(()) => info!("removed old local dump file {file}"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!("failed to remove old local dump file {file}: {e}"),
            }
        }
    }
}

impl fmt::Display for Zfs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Zfs {{ name: {:?}, cmd: {:?}, dataset: {:?}, full_when: {:?} }}",
            self.name,
            self.cmd,
            self.dataset,
            self.full_when.as_ref().map(|c| c.to_string())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(ymd: &str) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(
            ymd[0..4].parse().unwrap(),
            ymd[4..6].parse().unwrap(),
            ymd[6..8].parse().unwrap(),
            1,
            0,
            0,
        )
        .unwrap()
    }

    fn snap(name: &str, kind: SnapshotKind, ymd: &str) -> Snapshot {
        Snapshot {
            name: name.to_string(),
            kind,
            time: t(ymd),
        }
    }

    #[test]
    fn parse_snapshot_new_style() {
        let s = parse_snapshot("tank@snap-full-20260101-010000", "snap").unwrap();
        assert_eq!(s.kind, SnapshotKind::Full);
        assert_eq!(s.time, t("20260101"));

        let s = parse_snapshot("tank/data@snap-inc-20260102-020000", "snap").unwrap();
        assert_eq!(s.kind, SnapshotKind::Incremental);
        assert_eq!(s.name, "tank/data@snap-inc-20260102-020000");
    }

    #[test]
    fn parse_snapshot_legacy() {
        let s = parse_snapshot("tank@snap-20251231-235959", "snap").unwrap();
        assert_eq!(s.kind, SnapshotKind::Legacy);
    }

    #[test]
    fn parse_snapshot_rejects_foreign() {
        // Different base name.
        assert!(parse_snapshot("tank@other-20260101-010000", "snap").is_none());
        // Prefix collision: "snapx" is not "snap".
        assert!(parse_snapshot("tank@snapx-20260101-010000", "snap").is_none());
        // Malformed timestamp.
        assert!(parse_snapshot("tank@snap-full-20260101-01", "snap").is_none());
        // Missing @.
        assert!(parse_snapshot("tank", "snap").is_none());
        // Not our mark and not a valid timestamp: "snap-full" without ts.
        assert!(parse_snapshot("tank@snap-full", "snap").is_none());
    }

    #[test]
    fn dump_file_names() {
        assert_eq!(
            dump_file_name("snap", SnapshotKind::Full, "20260101-010000"),
            "snap-full-20260101-010000.snapshot"
        );
        assert_eq!(
            dump_file_name("snap", SnapshotKind::Incremental, "20260102-020000"),
            "snap-inc-20260102-020000.snapshot"
        );
    }

    #[test]
    fn send_args_full_and_inc() {
        assert_eq!(
            send_args(SnapshotKind::Full, None, "tank@snap-full-1"),
            vec!["send", "-R", "-v", "-c", "-L", "tank@snap-full-1"]
        );
        assert_eq!(
            send_args(
                SnapshotKind::Incremental,
                Some("tank@snap-full-1"),
                "tank@snap-inc-2"
            ),
            vec![
                "send",
                "-R",
                "-v",
                "-c",
                "-L",
                "-i",
                "tank@snap-full-1",
                "tank@snap-inc-2"
            ]
        );
    }

    #[test]
    fn plan_without_full_when_is_always_full() {
        let snaps = vec![snap(
            "tank@snap-inc-20260101-010000",
            SnapshotKind::Incremental,
            "20260101",
        )];
        assert_eq!(select_plan(t("20260201"), None, &snaps), vec!["full"]);
    }

    #[test]
    fn plan_first_run_is_full() {
        let cron = Cron::from_str("0 1 1 * *").unwrap();
        assert_eq!(select_plan(t("20260115"), Some(&cron), &[]), vec!["full"]);
    }

    #[test]
    fn plan_incremental_before_full_is_due() {
        let cron = Cron::from_str("0 1 1 * *").unwrap();
        // Last full on Jan 1; it is now Jan 15: next full (Feb 1) not due.
        let snaps = vec![
            snap(
                "tank@snap-full-20260101-010000",
                SnapshotKind::Full,
                "20260101",
            ),
            snap(
                "tank@snap-inc-20260114-010000",
                SnapshotKind::Incremental,
                "20260114",
            ),
        ];
        assert_eq!(
            select_plan(t("20260115"), Some(&cron), &snaps),
            vec![
                "inc".to_string(),
                "tank@snap-inc-20260114-010000".to_string()
            ]
        );
    }

    #[test]
    fn plan_full_when_due() {
        let cron = Cron::from_str("0 1 1 * *").unwrap();
        // Last full on Jan 1; it is now Feb 1: full is due.
        let snaps = vec![
            snap(
                "tank@snap-full-20260101-010000",
                SnapshotKind::Full,
                "20260101",
            ),
            snap(
                "tank@snap-inc-20260131-010000",
                SnapshotKind::Incremental,
                "20260131",
            ),
        ];
        assert_eq!(
            select_plan(t("20260201"), Some(&cron), &snaps),
            vec!["full"]
        );
    }

    #[test]
    fn plan_falls_back_to_full_without_base() {
        let cron = Cron::from_str("0 1 1 * *").unwrap();
        // Non-due time but no snapshots at all: incremental impossible.
        assert_eq!(select_plan(t("20260115"), Some(&cron), &[]), vec!["full"]);
    }

    #[test]
    fn cleanup_keeps_only_new_full() {
        let snaps = vec![
            snap(
                "tank@snap-full-20260101-010000",
                SnapshotKind::Full,
                "20260101",
            ),
            snap(
                "tank@snap-inc-20260102-010000",
                SnapshotKind::Incremental,
                "20260102",
            ),
            snap(
                "tank/data@snap-inc-20260102-010000",
                SnapshotKind::Incremental,
                "20260102",
            ),
            snap(
                "tank@snap-20251231-235959",
                SnapshotKind::Legacy,
                "20251231",
            ),
        ];
        let cleaned = cleanup_list("tank@snap-full-20260201-010000", &snaps);
        assert_eq!(
            cleaned,
            vec![
                "tank@snap-full-20260101-010000",
                "tank@snap-inc-20260102-010000",
                "tank/data@snap-inc-20260102-010000",
                "tank@snap-20251231-235959",
            ]
        );
    }

    #[test]
    fn parse_snapshot_name_file() {
        let s = parse_snapshot_name("snap-inc-20260102-020000.snapshot", "snap").unwrap();
        assert_eq!(s.kind, SnapshotKind::Incremental);
        assert_eq!(s.name, "snap-inc-20260102-020000.snapshot");
        assert!(parse_snapshot_name("snap-20260102-020000.snapshot", "snap").is_none());
        assert!(parse_snapshot_name("other-full-20260102-020000.snapshot", "snap").is_none());
    }

    #[test]
    fn old_dump_files_are_strictly_older() {
        let entries = vec![
            "snap-full-20260101-010000.snapshot".to_string(),
            "snap-inc-20260101-120000.snapshot".to_string(),
            "snap-full-20260102-010000.snapshot".to_string(), // the new full itself
            "other-full-20260101-010000.snapshot".to_string(), // not ours
            "readme.txt".to_string(),
        ];
        let old = old_dump_files(&entries, "snap", "snap-full-20260102-010000.snapshot");
        assert_eq!(
            old,
            vec![
                "snap-full-20260101-010000.snapshot".to_string(),
                "snap-inc-20260101-120000.snapshot".to_string(),
            ]
        );
    }

    #[test]
    fn parse_schedule_accepts_when_and_cron_formats() {
        assert!(parse_schedule("monthly 1 01:00").is_ok());
        assert!(parse_schedule("0 1 1 * *").is_ok());
        assert!(parse_schedule("nonsense").is_err());
    }

    #[test]
    fn required_keep_last_monthly_full_daily_backup() {
        // 31 day month: 30 incrementals between the fulls, plus the full.
        assert_eq!(
            required_keep_last("daily 01:00", "monthly 1 01:00").unwrap(),
            31
        );
        // Same schedules expressed as raw cron.
        assert_eq!(required_keep_last("0 1 * * *", "0 1 1 * *").unwrap(), 31);
    }

    #[test]
    fn required_keep_last_weekly_full_daily_backup() {
        assert_eq!(
            required_keep_last("daily 01:00", "weekly monday 01:00").unwrap(),
            7
        );
    }

    #[test]
    fn required_keep_last_daily_full() {
        assert_eq!(required_keep_last("daily 01:00", "daily 01:00").unwrap(), 1);
    }

    #[test]
    fn required_keep_last_invalid_expressions() {
        assert!(required_keep_last("nonsense", "monthly 1 01:00").is_err());
        assert!(required_keep_last("daily 01:00", "nonsense").is_err());
    }

    #[test]
    fn required_keep_last_unbounded_full_schedule() {
        // Never fires (February 31).
        assert!(required_keep_last("daily 01:00", "0 0 31 2 *").is_err());
        // Fires once in the window (February 29, leap year only).
        assert!(required_keep_last("daily 01:00", "0 0 29 2 *").is_err());
    }
}
