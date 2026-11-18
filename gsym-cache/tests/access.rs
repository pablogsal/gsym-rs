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
fn cache_creation_uses_the_resolved_parent_after_a_symlink_changes() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let base = directory
        .path()
        .canonicalize()
        .expect("temporary directory resolves");
    let target = base.join("target");
    let redirect = base.join("redirect");
    std::fs::create_dir(&target).expect("symlink target is created");
    symlink(&target, &redirect).expect("test symlink is created");
    let cache = Cache::open(redirect.join("gsym"), CacheEpoch::new(1))
        .expect("cache opens through the parent symlink");

    assert_eq!(cache.root(), target.join("gsym"));
    assert!(!target.join("gsym").exists());

    std::fs::remove_file(&redirect).expect("parent symlink is removed");
    symlink(&base, &redirect).expect("parent symlink has a new target");
    let build_id = BuildId::new([0x42; 20]).expect("test build identifier is valid");
    assert_eq!(
        cache.record_access(&build_id).expect("access is recorded"),
        AccessUpdate::Recorded
    );
    assert!(target.join("gsym").is_dir());
    assert!(!base.join("gsym").exists());
}

#[test]
fn cache_open_rejects_dangling_symlinks_without_creating_their_targets() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let target = directory.path().join("missing");
    let redirect = directory.path().join("redirect");
    symlink(&target, &redirect).expect("dangling symlink is created");

    for root in [redirect.clone(), redirect.join("gsym")] {
        assert!(Cache::open(root, CacheEpoch::new(1)).is_err());
    }
    assert!(!target.exists());
}

#[test]
fn cache_creation_rejects_symlinks_inside_the_cache() {
    use std::os::unix::fs::{DirBuilderExt as _, symlink};

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("cache root is created");
    let cache = Cache::open(&root, CacheEpoch::new(1)).expect("cache opens");
    symlink(directory.path(), root.join("v1")).expect("internal symlink is created");
    let build_id = BuildId::new([0x42; 20]).expect("test build identifier is valid");

    assert!(cache.record_access(&build_id).is_err());
    assert!(!directory.path().join("e1").exists());
}
