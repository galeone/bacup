# bacup

An easy to use backup tool designed for servers.

## Server

The server is the bacup service: it runs as a deamon and executes the backup of the **services** on the **remotes**.

The goal of bacup is to make the configuration straightforward: a single file where defining everything in a very simple way.


### Server configuration

3 steps configuration.

1. Configure the **remotes**. A remote is a cloud provider, or a SSH host, or a git server.
2. Configure the **services**. A service is a well-known software (e.g. PostgreSQL) with his own backup tool, or is a location on the filesystem.
3. Map remotes to services - configure the **backup**. Note: then hour:minutes part of the `when` field is **always** in GMT.

```toml
# remotes definitions
[aws]
    [aws.bucket_name]
    region = ""# e.g "eu-west-3"
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
    host = ""
    port = ""

[folders]
    [folders.service1]
    pattern = ""

# mapping services to remote
[backup]
    [backup.service1_db]
    what = "postgres.service1"
    where = "aws.bucket_name"
    when = "daily 01:00"
    remote_path = "/service1/database/"
    compress = false
    incremental = true

    [backup.service1_source]
    what = "postgres.service1"
    where = "aws.bucket_name"
    when = "daily 01:00"
    remote_path = "/service1/source/"
    compress = true
    incremental = false
```

## Remote configuration

Configuring the remotes is straightforward. Every remote have a different way of getting the access code, here we try to share some useful refrence.

### AWS

- Access Key & Secret Key: [https://docs.aws.amazon.com/general/latest/gr/aws-sec-cred-types.html#access-keys-and-secret-access-keys](Understanding and getting your AWS credentials: programmatic access)
- Region: the region is the region of your bucket.
