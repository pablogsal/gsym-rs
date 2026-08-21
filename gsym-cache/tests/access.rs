//! Access feature tests.
#![cfg(feature = "access")]

use gsym_cache::{AccessUpdate, BuildId, Cache, CacheEpoch};

#[test]
fn lightweight_access_tracking_records_then_debounces() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let cache =
        Cache::open(directory.path().join("cache"), CacheEpoch::new(1)).expect("test cache opens");
    let build_id = BuildId::new([0x42; 20]).expect("test build identifier is valid");

    assert_eq!(
        cache
            .record_access(&build_id)
            .expect("first access is recorded"),
        AccessUpdate::Recorded
    );
    assert_eq!(
        cache
            .record_access(&build_id)
            .expect("second access is checked"),
        AccessUpdate::Debounced
    );

    let short_build_id = BuildId::new([0x43]).expect("one-byte build identifier is valid");
    assert_eq!(
        cache
            .record_access(&short_build_id)
            .expect("short build-ID access is recorded"),
        AccessUpdate::Recorded
    );
}

#[test]
fn cache_creation_rejects_intermediate_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let target = directory.path().join("target");
    std::fs::create_dir(&target).expect("symlink target is created");
    symlink(&target, directory.path().join("redirect")).expect("test symlink is created");
    let result = Cache::open(directory.path().join("redirect/gsym"), CacheEpoch::new(1));

    assert!(result.is_err());
    assert!(!target.join("gsym").exists());
}
