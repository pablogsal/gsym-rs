//! Publishes a previously generated GSYM file under its build ID.

use std::fs::File;
use std::io;

use gsym_cache::{BuildId, Cache, CacheEpoch, PopulationOutcome};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = arguments
        .next()
        .ok_or("usage: populate ROOT BUILD_ID GSYM")?;
    let encoded = arguments
        .next()
        .ok_or("usage: populate ROOT BUILD_ID GSYM")?
        .into_string()
        .map_err(|_| "BUILD_ID must be UTF-8 hexadecimal")?;
    let source = arguments
        .next()
        .ok_or("usage: populate ROOT BUILD_ID GSYM")?;
    let build_id: BuildId = encoded.parse()?;
    let cache = Cache::open(root, CacheEpoch::new(1))?;

    match cache.try_begin_population(&build_id)? {
        PopulationOutcome::Present(entry) => {
            println!("already present: {} bytes", entry.len());
        }
        PopulationOutcome::Acquired(population) => {
            let mut source = File::open(source)?;
            let mut writer = population.into_writer()?;
            io::copy(&mut source, &mut writer)?;
            let outcome = writer.publish()?;
            println!(
                "{}: {} bytes",
                if outcome.is_published() {
                    "published"
                } else {
                    "racing publisher won"
                },
                outcome.entry().len()
            );
        }
        PopulationOutcome::Suppressed(failure) => {
            println!(
                "suppressed after {:?} until {:?}",
                failure.kind(),
                failure.expires_at()
            );
        }
        PopulationOutcome::Busy => println!("another population is active"),
    }
    Ok(())
}
