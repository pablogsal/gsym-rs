//! Prunes a cache to a byte limit and then scrubs its remaining metadata.

use gsym_cache::{ByteLimit, Cache, CacheEpoch, PrunePolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = arguments.next().ok_or("usage: maintain ROOT MAX_BYTES")?;
    let max_bytes = arguments
        .next()
        .ok_or("usage: maintain ROOT MAX_BYTES")?
        .into_string()
        .map_err(|_| "MAX_BYTES must be UTF-8")?
        .parse::<u64>()?;
    let cache = Cache::open(root, CacheEpoch::new(1))?;
    let policy = PrunePolicy::new(ByteLimit::try_from(max_bytes)?);

    let prune = cache.prune(policy)?;
    if let Some(report) = prune.into_report() {
        println!(
            "pruned {} entries; {} bytes remain",
            report.removed, report.after.bytes
        );
    } else {
        println!("maintenance is busy");
        return Ok(());
    }

    if let Some(report) = cache.scrub()?.into_report() {
        println!(
            "checked {}; removed {} corrupt entries",
            report.checked, report.removed_corrupt
        );
    }
    Ok(())
}
