//! Looks up a build ID and writes the cached GSYM bytes to standard output.

use std::io;

use gsym_cache::{BuildId, Cache, CacheEpoch};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = arguments.next().ok_or("usage: lookup ROOT BUILD_ID")?;
    let encoded = arguments
        .next()
        .ok_or("usage: lookup ROOT BUILD_ID")?
        .into_string()
        .map_err(|_| "BUILD_ID must be UTF-8 hexadecimal")?;
    let build_id: BuildId = encoded.parse()?;
    let cache = Cache::open(root, CacheEpoch::new(1))?;

    if let Some(entry) = cache.lookup(&build_id)? {
        io::copy(&mut entry.into_file(), &mut io::stdout().lock())?;
    }
    Ok(())
}
