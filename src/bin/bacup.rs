// Copyright 2022 Paolo Galeone <nessuno@nerdz.eu>
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

use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::Arc;

use std::string::String;

use bacup::backup::Backup;
use bacup::config::Config;

use bacup::remotes::aws::AwsBucket;
use bacup::remotes::git::Git;
use bacup::remotes::localhost::Localhost;
use bacup::remotes::ssh::Ssh;

use bacup::remotes::remote::Remote;

use bacup::services::docker::Docker;
use bacup::services::folders::Folder;
use bacup::services::postgresql::PostgreSql;
use bacup::services::service::Service;

use log::*;
use structopt::StructOpt;

use tokio_cron_scheduler::JobScheduler;

#[derive(StructOpt, Debug)]
#[structopt()]
struct Opt {
    /// Silence all output
    #[structopt(short = "q", long = "quiet")]
    quiet: bool,
    /// Verbose mode (-v, -vv, -vvv, etc)
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: usize,
}

#[tokio::main]
async fn main() -> Result<(), i32> {
    let opt = Opt::from_args();
    stderrlog::new()
        //.modules(vec![module_path!(), "bacup"])
        .quiet(opt.quiet)
        .verbosity(opt.verbose)
        .timestamp(stderrlog::Timestamp::Second)
        .init()
        .unwrap();

    let path = match env::var("CONF_FILE") {
        Ok(x) => x,
        Err(_) => "config.toml".to_string(),
    };

    let path = Path::new(&path);

    if !path.exists() {
        error!("The configuration file {:?} doesn't exist.", path);
        return Err(-1);
    }

    let config = match Config::new(path).await {
        Ok(config) => config,
        Err(error) => {
            error!("Config error: {}", error);
            return Err(-1);
        }
    };

    let mut remotes: HashMap<String, Box<dyn Remote + Send + Sync>> = HashMap::new();

    match config.aws {
        Some(aws) => {
            for (bucket_name, bucket_config) in aws {
                remotes.insert(
                    format!("aws.{}", bucket_name),
                    Box::new(AwsBucket::new(bucket_config, &bucket_name).await.unwrap()),
                );
                info!("Remote aws.{} configured", bucket_name);
            }
        }
        None => warn!("No AWS cloud configured."),
    }

    match config.ssh {
        Some(host) => {
            for (hostname, config) in host {
                remotes.insert(
                    format!("ssh.{}", hostname),
                    Box::new(Ssh::new(config, &hostname).await.unwrap()),
                );
                info!("Remote ssh.{} configured", hostname);
            }
        }
        None => warn!("No Ssh remotes configured."),
    }

    match config.localhost {
        Some(host) => {
            for (name, config) in host {
                remotes.insert(
                    format!("localhost.{}", name),
                    Box::new(Localhost::new(config, &name).unwrap()),
                );
                info!("Remote localhost.{} configured", name);
            }
        }
        None => warn!("No localhost remotes configured."),
    }

    match config.git {
        Some(host) => {
            for (name, config) in host {
                remotes.insert(
                    format!("git.{}", name),
                    Box::new(Git::new(config, &name).await.unwrap()),
                );
                info!("Remote git.{} configured", name);
            }
        }
        None => warn!("No Git remotes configured."),
    }

    let mut services: HashMap<String, Box<dyn Service + Send + Sync>> = HashMap::new();
    match config.folders {
        Some(folders) => {
            for (location_name, folder) in folders {
                let key = format!("folders.{}", location_name);
                services.insert(key, Box::new(Folder::new(&folder.pattern).await.unwrap()));
            }
        }
        None => warn!("No folders to backup."),
    }
    match config.postgres {
        Some(postgres) => {
            for (service_name, instance_config) in postgres {
                let key = format!("postgres.{}", service_name);
                services.insert(
                    key,
                    Box::new(
                        PostgreSql::new(instance_config, &service_name)
                            .await
                            .unwrap(),
                    ),
                );
            }
        }
        None => warn!("No PostgreSql to backup."),
    }
    match config.docker {
        Some(docker) => {
            for (service_name, instance_config) in docker {
                let key = format!("docker.{}", service_name);
                services.insert(
                    key,
                    Box::new(Docker::new(instance_config, &service_name).await.unwrap()),
                );
            }
        }
        None => warn!("No Docker to backup."),
    }

    let mut backup: HashMap<String, Arc<Backup>> = HashMap::new();
    for (backup_name, config) in config.backup {
        if !services.contains_key(&config.what) {
            error!(
                "Backup {}. Invalid what: {}, not available in the configured services: {:?}",
                backup_name,
                config.what,
                services.keys()
            );
            return Err(-1);
        }

        if !remotes.contains_key(&config.r#where) {
            error!(
                "Backup {}. Invalid where: {}, not available in the configured remotes: {:?}",
                backup_name,
                config.r#where,
                remotes.keys()
            );
            return Err(-1);
        }

        backup.insert(
            backup_name.clone(),
            Arc::new(
                Backup::new(
                    &backup_name,
                    dyn_clone::clone_box(&*remotes[&config.r#where]),
                    dyn_clone::clone_box(&*services[&config.what]),
                    &config,
                )
                .await
                .unwrap(),
            ),
        );
        info!("Backup {} -> {} configured", config.what, config.r#where);
    }

    let mut scheduler = JobScheduler::new().unwrap();
    // scheduler.shutdown_on_ctrl_c();

    for (name, job) in backup {
        let upcoming = job.schedule.upcoming(chrono::Utc).take(1).next().unwrap();
        let schedule = job.schedule.clone();
        let res = job.schedule(&mut scheduler, schedule).await;

        match res {
            Err(error) => {
                error!("Error during scheduling: {:?}", error);
                return Err(-1);
            }
            Ok(uuid) => info!(
                "Successfully scheduled {} ({}). Next run: {}",
                name, uuid, upcoming
            ),
        }
    }

    if scheduler.start().is_err() {
        error!("Unable to start the scheduler");
        return Err(-1);
    }
    use tokio::time::Duration;
    loop {
        /*if let Err(e) = scheduler.tick() {
            error!("Scheduler tick error: {:?}", e);
            return Err(-1);
        }
        */
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
