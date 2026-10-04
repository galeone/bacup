# bacup

An easy-to-use backup tool designed for servers - written in Rust.

---

The bacup service runs as a deamon and executes the backup of the **services** on the **remotes**.

The goal of bacup is to make the configuration straightforward: a single file where defining everything in a very simple way.

## Configuration

3 steps configuration.

1. Configure the **remotes**. A remote is a cloud provider, or a SSH host, or a git server.
2. Configure the **services**. A service is a well-known software (e.g. PostgreSQL) with his own backup tool, or is a location on the filesystem.
3. Map services (**what** to backup) to remotes (**where** to backup). Configure the **backup**.

When configuring the backups, the field **when** accepts configuration strings in the format:

- `"daily $hh:$mm` e.g. `daily 15:30`
- `weekly $day $hh:$mm` e.g. `weekly mon 12:23` or `weekly monday 12:23`. `weekly` can be omitted.
- `monthly $day $hh:$mm` e.g. `monthly 1 00:30`
- **cron**. If you really have to use it, use [crontab guru](https://crontab.guru/) to create the cron string.

**NOTE**: The time is ALWAYS in UTC timezone.

```toml
# remotes definitions
[aws]
    [aws.bucket_name]
    region = ""# "eu-west-3"
    access_key = ""
    secret_key = ""

# Not available yet!
#[gcloud]
#    [gcloud.bucket1]
#    service_account_path = ""

[ssh]
    [ssh.remote_host1]
    host = "" # example.com
    port = "" # 22
    username = "" # myname
    private_key = "" # ~/.ssh/id_rsa

[localhost]
    # Like copy-paste in local. The underlying infrastructure manages
    # the remote (if any) part. Below 2 examples
    [localhost.samba]
    path = "" # local path where samba is mounted

    [localhost.disk2]
    path = "" # local path where the second disk of the machine is mounted

[git]
    [git.remote_repo]
    host = "" #github.com
    port = "" #22
    username = "" #git
    private_key = "" # ~/.ssh/id_rsa
    repository = "" # "galeone/bacup"
    branch = "" # master

# what to backup. Service definition
[postgres]
    [postgres.service1]
    username = ""
    db_name = ""
    host = ""
    port = ""

[folders]
    [folders.service1]
    pattern = ""

[docker]
    [docker.service]
    container_name = "docker_postgres_1"
    command = "pg_dumpall -c -U postgres" # dump to stdout always

[zfs]
    # the user needs zfs permissions on the datasets (bacup verifies them at startup):
    # zfs allow $USER destroy,mount,send,snapshot <dataset>
    # (permissions are inherited by child datasets)
    [zfs.root]
    snapshot_name = "root-fs"
    dataset = "zroot"
    [zfs.storage]
    snapshot_name = "storage-fs"
    dataset = "storage"
    # optional: how often a full backup is taken. Accepts the same format
    # as the `when` field (e.g. "monthly 1 01:00") or a raw cron expression.
    # The runs in between take incremental backups against the latest
    # snapshot. When omitted every run is a full backup.
    #full_when = "monthly 1 01:00"

# mapping services to remote
[backup]
    # Compress the DB dump and upload it to aws
    # everyday at 01:00 UTC
    [backup.service1_db_compress]
    what = "postgres.service1"
    where = "aws.bucket_name"
    when = "daily 01:00"
    remote_path = "/service1/database/"
    compress = true
    keep_last = 7

    # Dump the DB and upload it to aws (no compression)
    # every first day of the month
    [backup.service1_db]
    what = "postgres.service1"
    where = "aws.bucket_name"
    when = "monthly 1 00:00"
    remote_path = "/service1/database/"
    compress = false

    # Archive the files of service 1 and upload them to
    # the ssh.remote_host1 in the remote ~/backups/service1 folder.
    # Every friday at 5:00
    [backup.service1_source_compress]
    what = "folders.service1"
    where = "ssh.remote_host1"
    when = "weekly friday 05:00"
    remote_path = "~/backups/service1"
    compress = true

    # Incrementally sync folders.service1 with the remote host
    # using rsync (authenticated trough ssh)
    # At 00:05 in August
    [backup.service1_source]
    what = "folders.service1"
    where = "ssh.remote_host1"
    when = "5 0 * 8 *"
    remote_path = "~/backups/service1_incremental/"
    compress = false # no compression = incremental sync

    # Compress the DB dump and copy it to the localhost "remote"
    # where, for example, samba is mounted
    # everyday at 01:00 UTC
    [backup.service1_db_on_samba]
    what = "postgres.service1"
    where = "localhost.samba"
    when = "daily 01:00"
    remote_path = "/path/inside/the/samba/location"
    compress = false

    [backup.service1_source_git]
    what = "folders.service1"
    where = "git.github"
    when = "daily 15:30"
    remote_path = "/" # the root of the repo
    compress = false
```

When `compression = true`, the file/folder are compressed using Gzip and the file is archived (in the desired remote location) with the format:

```
YYYY-MM-DD-hh:mm-filename.gz # or .tar.gz if filename is an archive
```

## ZFS backups

The `zfs` service backs up a dataset tree with `zfs snapshot` + `zfs send`. Every service produces one dump file per run, which is then uploaded to the remote like any other backup: the schedule, `remote_path` and `keep_last` are the usual `[backup.<name>]` fields (`what = "zfs.<service>"`).

### Full and incremental backups

- On every run bacup creates a snapshot `dataset@snapshot_name-<kind>-<timestamp>` on the dataset and all of its children, then `zfs send -R` writes it to the working directory as `<service>-<kind>-<timestamp>.snapshot` (`<kind>` is `full` or `inc`).
- Without `full_when` every run is a full backup.
- With `full_when` set, a full backup is taken when the schedule is due (the first run is always a full) and the runs in between are **incrementals** against the latest existing snapshot of the chain, so they only contain what changed since the last run and stay small.
- If an incremental can't be sent (e.g. a child dataset was destroyed and recreated), bacup destroys the incremental snapshot and retries the run as a full backup, so a run never fails silently.
- After each **full** backup the previous chain is replaced: all older snapshots of the service (including those on child datasets) and the older local dump files are destroyed. On the remote, dump files are pruned by `keep_last` as usual.
- The dump is produced with `zfs send -c -L`: blocks that are compressed on disk stay compressed in the dump file (the `-c` flag needs OpenZFS >= 2.1.1 on the sender). If the dataset uses compression (check with `zfs get -o value compression <dataset>`), set `compress = false` on the backup — gzip'ing an already-compressed stream at upload time is wasted CPU with no size gain. Keep `compress = true` only for uncompressed datasets. bacup logs a hint at startup when the dataset is compressed.

### Restoring

Dump files are standard `zfs receive` streams. To restore, take the newest full and then apply every incremental after it, in timestamp order:

```
zfs receive -F targetds < <service>-full-20260901-010000.snapshot
zfs receive -F targetds < <service>-inc-20260902-010000.snapshot
zfs receive -F targetds < <service>-inc-20260903-010000.snapshot
```

Dumps of compressed datasets carry compressed blocks (sent with `-c`), so the receiving pool must have the matching compression features enabled (`lz4_compress`/`zstd_compress`) — i.e. the same or a newer ZFS than the original pool.

A restore therefore needs the newest full and every incremental taken after it. If you plan to restore from the remote, size `keep_last` so it can never prune a full that incrementals on top of it still need.

bacup enforces this at startup: for every zfs backup with a `full_when`, it computes the longest stretch of incremental runs between two fulls from the two schedules and refuses to start if `keep_last` is set below what keeps a full and all its incrementals. Without `keep_last` nothing is pruned, so nothing can break the chain.

## Installation & service setup

```
cargo install bacup
```

Then put the `config.toml` file in `$HOME/.bacup/config.toml`.

There's a ready to use `systemd` service file:

```
sudo cp misc/systemd/bacup@.service /usr/lib/systemd/system/
```

then, the service can be enabled/started in the usual systemd way:

```
sudo systemctl enable --now bacup@$USER.service
```

**Note**: the working directory is important if you plan to back-up big files. The files are created in that directory before being uploaded, so set it to a location where you have the write right and enough space.

## Development

To test an unreleased branch, build and install it from your local checkout — it replaces the crates.io binary in `~/.cargo/bin`:

```
git clone https://github.com/galeone/bacup
cd bacup
git checkout <branch>
cargo install --path .
```

Useful before wiring the new version into the systemd service, or to quickly verify a fix on a real backup. When a release you like is out, `cargo install bacup` puts the released binary back.

Alternatively, `cargo build --release` leaves the binary at `target/release/bacup` without touching the installed one — handy for a one-off run (it still reads `$HOME/.bacup/config.toml`).

## Remote configuration

Configuring the remotes is straightforward. Every remote have a different way of getting the access code, here we try to share some useful reference.

### AWS

- Access Key & Secret Key: [Understanding and getting your AWS credentials: programmatic access](https://docs.aws.amazon.com/general/latest/gr/aws-sec-cred-types.html#access-keys-and-secret-access-keys)
- Region: the region is the region of your bucket.
- Endpoint: (optional) the endpoint to use for the client, i.e. another s3 compatible service.
- force_path_style: (optional) Forces this client to use path-style addressing for buckets, necessary for some s3 compatible gateways.

### SSH

You need a valid ssh account on your remote - only authentication via SSH key without passphrase is supported.

For incremental backup `rsync` is used - you need this tool installed locally and remotely.

### Git

You need a valid account on a Git server, together with a repository. Only SSH is supported.

### Localhost

Not properly a remote, but you can use `bacup` to bacup from a path to another (with/without compression). If the localhost remote is mounted on a network filesystem it's better :)
