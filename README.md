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
sudo systemctl start bacup@$USER.service
sudo systemctl enable bacup@$USER.service
```

## Remote configuration

Configuring the remotes is straightforward. Every remote have a different way of getting the access code, here we try to share some useful reference.

### AWS

- Access Key & Secret Key: [Understanding and getting your AWS credentials: programmatic access](https://docs.aws.amazon.com/general/latest/gr/aws-sec-cred-types.html#access-keys-and-secret-access-keys)
- Region: the region is the region of your bucket.

### SSH

You need a valid ssh account on your remote - only authentication via SSH key without passphrase is supported.

For incremental backup `rsync` is used - you need this tool installed locally and remotely.

### Git

You need a valid account on a Git server, together with a repository. Only SSH is supported.

### Localhost

Not properly a remote, but you can use `bacup` to bacup from a path to another (with/without compression). If the localhost remote is mounted on a network filesystem it's better :)
