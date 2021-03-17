use std::env;
use std::path::Path;

use bacup::aws;
use bacup::config::Config;
use log::{error, info};

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

    let config = match Config::read(path) {
        Ok(config) => config,
        Err(error) => {
            error!("Config error: {}", error);
            return Err(-1);
        }
    };

    let mut clouds = std::collections::HashMap::new();

    match config.aws {
        Some(aws) => {
            for (bucket_name, bucket_config) in aws {
                clouds.insert(
                    format!("aws.{}", bucket_name),
                    aws::AWSBucket::new(bucket_config, &bucket_name).unwrap(),
                );
            }
        }
        None => info!("No AWS cloud configured."),
    }

    Ok(())
}
