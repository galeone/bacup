use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::string::String;

use bacup::backup::Backup;
use bacup::config::Config;
use bacup::remotes::aws::AWSBucket;
use bacup::remotes::uploader::Uploader;
use bacup::services::folders::Folder;
use bacup::services::postgresql::PostgreSQL;
use bacup::services::service::Service;

use log::*;
use structopt::StructOpt;

use job_scheduler::JobScheduler;

use std::time::Duration;

use dyn_clone;

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

    let config = match Config::new(path) {
        Ok(config) => config,
        Err(error) => {
            error!("Config error: {}", error);
            return Err(-1);
        }
    };

    let mut remotes: HashMap<String, Box<dyn Uploader>> = HashMap::new();

    match config.aws {
        Some(aws) => {
            for (bucket_name, bucket_config) in aws {
                remotes.insert(
                    format!("aws.{}", bucket_name),
                    Box::new(AWSBucket::new(bucket_config, &bucket_name).await.unwrap()),
                );
            }
        }
        None => warn!("No AWS cloud configured."),
    }

    let mut services: HashMap<String, Box<dyn Service>> = HashMap::new();
    match config.folders {
        Some(folders) => {
            for (location_name, folder) in folders {
                services.insert(
                    format!("folders.{}", location_name),
                    Box::new(Folder::new(&folder.pattern).unwrap()),
                );
            }
        }
        None => warn!("No folders to backup."),
    }
    match config.postgres {
        Some(postgres) => {
            for (service_name, instance_config) in postgres {
                services.insert(
                    format!("postgres.{}", service_name),
                    Box::new(PostgreSQL::new(instance_config, &service_name).unwrap()),
                );
            }
        }
        None => warn!("No PostgreSQL to backup."),
    }

    let mut backup: HashMap<String, Backup> = HashMap::new();
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
            Backup::new(
                &backup_name,
                dyn_clone::clone_box(&*remotes[&config.r#where]),
                dyn_clone::clone_box(&*services[&config.what]),
                config,
            )
            .unwrap(),
        );
    }

    let mut scheduler = JobScheduler::new();

    for (name, job) in backup {
        let upcoming = job.schedule.upcoming(chrono::Utc).take(1).next().unwrap();
        let schedule = job.schedule.clone();
        let res = job.schedule(&mut scheduler, schedule);

        match res {
            Err(error) => {
                error!("Error during scheduling: {}", error);
                return Err(-1);
            }
            Ok(()) => info!("Successfully scheduled {}. Next run: {}", name, upcoming),
        }
    }

    loop {
        scheduler.tick();
        std::thread::sleep(Duration::from_millis(500));
    }
}
