# bacup

An easy to use backup tool designed for servers.

## Server

The server is the bacup service: it runs as a deamon and executes the backup of the **services** on the **remotes**.

The goal of bacup is to make the configuration straightforward: a single file where defining everything in a very simple way.


### Server configuration

3 steps configuration.

1. Configure the **remotes**. A remote is a cloud provider, or a SSH host, or a git server.
2. Configure the **services**. A service is a well-known software (e.g. PostgreSQL) with his own backup tool, or is a location on the filesystem.
3. Map remotes to services - configure the **backup**.



```toml
# remotes definitions
[aws]
    [aws.bucket_name]
    region = ""
    access_key = ""
    secret_key = ""

[gcloud]
    [gcloud.bucket1]
    service_account_path = ""

[ssh]
    [ssh.remote_host1]
    host = ""
    port = ""
    username = ""

[git]
    [git.github]
    host = ""
    port = ""
    username = ""

# what to backup. Service definition
[postgres]
    [postgres.service1]
    username = ""
    db_name = ""

[folders]
    [folders.service1]
    pattern = ""

# mapping services to remote
[backup]
    [backup.service1_db]
    what = "postgres.service1"
    where = "aws.bucket_name"
    when = "daily 01:00 GMT"
    remote_path = "/service1/database/"
    compress = "no"
    incremental = "yes"

    [backup.service1_source]
    what = "postgres.service1"
    where = "aws.bucket_name"
    when = "daily 01:00 GMT"
    remote_path = "/service1/source/"
    compress = "yes"
    incremental = "no"
```
