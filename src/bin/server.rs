use std::collections::HashMap;
use std::env;
use std::path::Path;

use bacup::config::Config;
use bacup::remotes::aws;
use bacup::services::folders;
use log::{error, warn};

use structopt::StructOpt;

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

fn main() -> Result<(), i32> {
    let opt = Opt::from_args();

    stderrlog::new()
        .module(module_path!())
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

    let mut remotes = HashMap::new();

    match config.aws {
        Some(aws) => {
            for (bucket_name, bucket_config) in aws {
                remotes.insert(
                    format!("aws.{}", bucket_name),
                    aws::AWSBucket::new(bucket_config, &bucket_name).unwrap(),
                );
            }
        }
        None => warn!("No AWS cloud configured."),
    }

    let mut services = HashMap::new();
    match config.folders {
        Some(folders) => {
            for (location_name, folder) in folders {
                services.insert(
                    format!("folders.{}", location_name),
                    folders::Folder::new(&folder.pattern).unwrap(),
                );
            }
        }
        None => warn!("No folders to backup configured."),
    }

    Ok(())
}
