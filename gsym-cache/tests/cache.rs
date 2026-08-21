//! Managed cache tests.
#![cfg(feature = "manage")]

use std::io::{Read as _, Write as _};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gsym::{AddressRange, Function, Gsym, GsymBuilder, GsymVersion};
use gsym_cache::{
    AccessUpdate, BuildId, ByteLimit, Cache, CacheEpoch, FailureKind, MAX_FAILURE_TTL,
    PopulationOutcome, PruneOutcome, PrunePolicy, PublishOutcome, ScrubOutcome,
};

fn identifier(byte: u8) -> BuildId {
    BuildId::new([byte; 20]).expect("test build identifier is valid")
}

fn cache(root: &std::path::Path) -> Cache {
    Cache::open(root, CacheEpoch::new(1)).expect("test cache opens")
}

fn population<'cache>(
    cache: &'cache Cache,
    build_id: &'cache BuildId,
) -> gsym_cache::Population<'cache> {
    for attempt in 0..100 {
        match cache
            .try_begin_population(build_id)
            .expect("population lock can be inspected")
        {
            PopulationOutcome::Acquired(population) => return population,
            PopulationOutcome::Busy if attempt < 99 => {
                std::thread::sleep(Duration::from_millis(1));
            }
            other @ (PopulationOutcome::Busy
            | PopulationOutcome::Suppressed(_)
            | PopulationOutcome::Present(_)) => {
                panic!(
                    "expected population ownership for {} in {}, got {other:?}",
                    build_id,
                    cache.root().display()
                )
            }
        }
    }
    unreachable!("the final retry returns or panics")
}

fn object_path(root: &std::path::Path, build_id: &BuildId) -> std::path::PathBuf {
    metadata_path(root, "objects", build_id, ".gsym")
}

fn temporary_directory(root: &std::path::Path, build_id: &BuildId) -> std::path::PathBuf {
    metadata_path(root, "tmp", build_id, ".tmp")
}

fn metadata_path(
    root: &std::path::Path,
    kind: &str,
    build_id: &BuildId,
    suffix: &str,
) -> std::path::PathBuf {
    let encoded = build_id.to_string();
    let (prefix, rest) = encoded
        .split_at_checked(2)
        .expect("a validated build identifier has a one-byte prefix");
    root.join("v1/e1")
        .join(kind)
        .join(".build-id")
        .join(prefix)
        .join(format!("{rest}{suffix}"))
}

fn rewrite_negative_expiration(path: &std::path::Path, expiration: SystemTime) {
    use std::os::unix::fs::PermissionsExt as _;

    let mut record = std::fs::read(path).expect("negative entry is readable");
    record[8..].copy_from_slice(
        &expiration
            .duration_since(UNIX_EPOCH)
            .expect("test expiration follows the epoch")
            .as_secs()
            .to_le_bytes(),
    );
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("negative entry is made writable");
    std::fs::write(path, record).expect("negative entry is rewritten");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))
        .expect("negative entry is made read-only");
}

fn publish(cache: &Cache, build_id: &BuildId, name_len: usize) {
    let population = population(cache, build_id);
    let mut writer = population
        .into_writer()
        .expect("population writer is created");
    let mut builder = GsymBuilder::new().build_id(build_id.as_bytes());
    builder
        .add_function(Function::new(
            AddressRange::new(0x1000, 0x1010),
            vec![b'x'; name_len],
        ))
        .expect("test function is valid");
    builder.write_to(&mut writer).expect("test GSYM is written");
    let outcome = writer.publish().expect("test GSYM is published");
    assert!(outcome.is_published());
    assert!(!outcome.entry().is_empty());
    drop(outcome.into_entry());
}

#[test]
fn missing_cache_is_a_miss_without_creating_directories() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    assert!(matches!(cache.lookup(&identifier(1)), Ok(None)));
    assert!(!root.exists());
}

#[test]
fn rejects_a_nonprivate_cache_root() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    std::fs::create_dir(&root).expect("cache root is created");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o750))
        .expect("cache root permissions are changed");

    assert!(matches!(
        Cache::open(root, CacheEpoch::new(1)),
        Err(gsym_cache::Error::InsecureDirectory { .. })
    ));
}

#[test]
fn lookup_rejects_a_symlinked_build_id_shard() {
    use std::os::unix::fs::{DirBuilderExt as _, symlink};

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let objects = root.join("v1/e1/objects/.build-id");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&objects)
        .expect("cache directories are created");

    let build_id = identifier(0x12);
    let object = object_path(&root, &build_id);
    let filename = object.file_name().expect("object has a filename");
    let redirect = directory.path().join("redirect");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&redirect)
        .expect("redirect directory is created");
    std::fs::write(redirect.join(filename), b"not a cache entry")
        .expect("redirected object is created");
    symlink(&redirect, object.parent().expect("object has a shard"))
        .expect("shard symlink is created");

    assert!(cache(&root).lookup(&build_id).is_err());
}

#[test]
fn validates_build_identifier_bounds() {
    assert!(BuildId::new([]).is_err());
    assert!(BuildId::new([0xaa; 126]).is_ok());
    assert!(BuildId::new([0xaa; 127]).is_err());
    let parsed: BuildId = "Aa01fF".parse().expect("hexadecimal build ID is valid");
    assert_eq!(parsed.as_bytes(), &[0xaa, 0x01, 0xff]);
    assert_eq!(parsed.to_string(), "aa01ff");
    assert!("abc".parse::<BuildId>().is_err());
    assert!("zz".parse::<BuildId>().is_err());
    assert!("aa".repeat(127).parse::<BuildId>().is_err());
}

#[test]
fn streams_verifies_and_publishes_a_read_only_entry() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(2);
    publish(&cache, &build_id, 64);

    let Some(entry) = cache.lookup(&build_id).expect("published entry is found") else {
        panic!("published entry was missing");
    };
    let mut bytes =
        Vec::with_capacity(usize::try_from(entry.len()).expect("test file fits memory"));
    entry
        .into_file()
        .read_to_end(&mut bytes)
        .expect("published entry is readable");
    let gsym = Gsym::parse(bytes).expect("published entry parses");
    assert_eq!(gsym.build_id(), build_id.as_bytes());

    let path = object_path(&root, &build_id);
    let mode = std::fs::metadata(path)
        .expect("published entry has metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o400);
}

#[test]
fn published_outcome_does_not_retain_write_access() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let cache = cache(&directory.path().join("cache"));
    let build_id = identifier(3);
    let population = population(&cache, &build_id);
    let mut writer = population
        .into_writer()
        .expect("population writer is created");
    let mut builder = GsymBuilder::new().build_id(build_id.as_bytes());
    builder
        .add_function(Function::new(
            AddressRange::new(0x1000, 0x1010),
            b"readonly".to_vec(),
        ))
        .expect("test function is valid");
    builder.write_to(&mut writer).expect("test GSYM is written");

    let outcome = writer.publish().expect("test GSYM is published");
    assert!(outcome.entry().file().set_len(0).is_err());
}

#[test]
fn publishes_v2_with_the_longest_cache_build_identifier() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let cache = cache(&directory.path().join("cache"));
    let build_id = BuildId::new([0xab; 126]).expect("maximum build identifier is valid");
    let population = population(&cache, &build_id);
    let mut writer = population
        .into_writer()
        .expect("population writer is created");
    let mut builder = GsymBuilder::new()
        .version(GsymVersion::V2)
        .build_id(build_id.as_bytes());
    builder
        .add_function(Function::new(AddressRange::new(1, 2), b"v2"))
        .expect("test function is valid");
    builder.write_to(&mut writer).expect("v2 GSYM is written");
    assert!(matches!(writer.publish(), Ok(PublishOutcome::Published(_))));
    assert!(matches!(cache.lookup(&build_id), Ok(Some(_))));
}

#[test]
fn rejects_a_staged_file_with_the_wrong_build_identifier() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let expected = identifier(3);
    let actual = vec![4; 4096];
    let population = population(&cache, &expected);
    let mut writer = population
        .into_writer()
        .expect("population writer is created");
    let mut builder = GsymBuilder::new()
        .version(GsymVersion::V2)
        .build_id(actual.clone());
    builder
        .add_function(Function::new(AddressRange::new(1, 2), b"wrong"))
        .expect("test function is valid");
    builder.write_to(&mut writer).expect("test GSYM is written");
    let error = writer
        .publish()
        .expect_err("mismatched GSYM must not be published");
    let gsym_cache::Error::BuildIdMismatch(mismatch) = error else {
        panic!("unexpected publication error: {error}");
    };
    let staging_directory = temporary_directory(&root, &expected);
    assert_eq!(mismatch.path().parent(), Some(staging_directory.as_path()));
    assert_eq!(mismatch.expected(), &expected);
    assert_eq!(mismatch.actual_len(), actual.len());
    assert_eq!(mismatch.actual_prefix(), &actual[..32]);
    assert!(matches!(cache.lookup(&expected), Ok(None)));
}

#[test]
fn population_lock_is_process_safe() {
    const CHILD_ROOT: &str = "GSYM_CACHE_LOCK_TEST_ROOT";

    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let cache = cache(std::path::Path::new(&root));
        let build_id = identifier(5);
        assert!(matches!(
            cache.try_begin_population(&build_id),
            Ok(PopulationOutcome::Busy)
        ));
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(5);
    let _population = population(&cache, &build_id);
    let output = Command::new(std::env::current_exe().expect("test executable is known"))
        .args(["--exact", "population_lock_is_process_safe", "--nocapture"])
        .env(CHILD_ROOT, &root)
        .output()
        .expect("child test process runs");
    assert!(
        output.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn completed_population_is_rechecked_after_locking() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let cache = cache(&directory.path().join("cache"));
    let build_id = identifier(6);
    publish(&cache, &build_id, 8);
    assert!(matches!(
        cache.try_begin_population(&build_id),
        Ok(PopulationOutcome::Present(_))
    ));
}

#[test]
fn access_updates_are_debounced_without_touching_the_object() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(7);
    publish(&cache, &build_id, 8);
    let object = object_path(&root, &build_id);
    let before = std::fs::metadata(&object)
        .and_then(|metadata| metadata.modified())
        .expect("object mtime is available");
    assert_eq!(
        cache.record_access(&build_id).expect("access is recorded"),
        AccessUpdate::Recorded
    );
    assert_eq!(
        cache.record_access(&build_id).expect("access is debounced"),
        AccessUpdate::Debounced
    );
    let after = std::fs::metadata(object)
        .and_then(|metadata| metadata.modified())
        .expect("object mtime remains available");
    assert_eq!(before, after);
}

#[test]
fn future_access_markers_are_repaired() {
    use std::fs::FileTimes;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(17);
    publish(&cache, &build_id, 8);
    assert_eq!(
        cache
            .record_access(&build_id)
            .expect("access marker is created"),
        AccessUpdate::Recorded
    );
    let marker = metadata_path(&root, "access", &build_id, ".lru");
    let future = SystemTime::now()
        .checked_add(Duration::from_secs(24 * 60 * 60))
        .expect("test timestamp is representable");
    std::fs::File::open(&marker)
        .and_then(|file| file.set_times(FileTimes::new().set_modified(future)))
        .expect("access marker is moved into the future");

    assert_eq!(
        cache
            .record_access(&build_id)
            .expect("future marker is repaired"),
        AccessUpdate::Recorded
    );
}

#[test]
fn rejects_a_nonregular_access_marker() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(21);
    publish(&cache, &build_id, 8);
    assert_eq!(
        cache
            .record_access(&build_id)
            .expect("access marker is created"),
        AccessUpdate::Recorded
    );
    let marker = metadata_path(&root, "access", &build_id, ".lru");
    std::fs::remove_file(&marker).expect("access marker is removed");
    std::fs::create_dir(&marker).expect("directory replaces access marker");

    assert!(matches!(
        cache.record_access(&build_id),
        Err(gsym_cache::Error::UntrustedEntry { .. })
    ));
}

#[test]
fn negative_entries_are_typed_and_expire() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(8);
    let expires = SystemTime::now()
        .checked_add(Duration::from_secs(60))
        .expect("test expiration is representable");
    population(&cache, &build_id)
        .record_failure_for(FailureKind::MissingInput, Duration::from_secs(60))
        .expect("negative entry is recorded");
    let failure = cache
        .cached_failure(&build_id)
        .expect("negative entry is readable")
        .expect("negative entry has not expired");
    assert_eq!(failure.kind(), FailureKind::MissingInput);
    assert!(failure.expires_at() >= expires);
    assert!(
        failure
            .expires_at()
            .duration_since(expires)
            .is_ok_and(|rounding| rounding < Duration::from_secs(1))
    );
    assert!(matches!(
        cache.try_begin_population(&build_id),
        Ok(PopulationOutcome::Suppressed(cached)) if cached.kind() == FailureKind::MissingInput
    ));

    let expired_build_id = identifier(19);
    assert!(matches!(
        population(&cache, &expired_build_id)
            .record_failure_for(FailureKind::TransientIo, Duration::ZERO,),
        Err(gsym_cache::Error::InvalidFailureTtl { .. })
    ));
    population(&cache, &expired_build_id)
        .record_failure_for(FailureKind::TransientIo, Duration::from_secs(60))
        .expect("negative entry is recorded");
    let expired_path = metadata_path(&root, "negative", &expired_build_id, ".neg");
    rewrite_negative_expiration(
        &expired_path,
        UNIX_EPOCH
            .checked_add(Duration::from_secs(1))
            .expect("test expiration is representable"),
    );
    assert!(matches!(cache.cached_failure(&expired_build_id), Ok(None)));
    drop(population(&cache, &expired_build_id));

    let distant_build_id = identifier(22);
    assert!(matches!(
        population(&cache, &distant_build_id).record_failure_for(
            FailureKind::MissingInput,
            MAX_FAILURE_TTL + Duration::from_secs(60),
        ),
        Err(gsym_cache::Error::InvalidFailureTtl { .. })
    ));

    let maximum_build_id = identifier(27);
    population(&cache, &maximum_build_id)
        .record_failure_for(FailureKind::ResourceExhausted, MAX_FAILURE_TTL)
        .expect("maximum negative-cache lifetime is accepted");
    assert!(matches!(
        cache.cached_failure(&maximum_build_id),
        Ok(Some(failure)) if failure.kind() == FailureKind::ResourceExhausted
    ));

    let forged_build_id = identifier(23);
    population(&cache, &forged_build_id)
        .record_failure_for(FailureKind::MissingInput, Duration::from_secs(60))
        .expect("negative entry is recorded");
    let path = metadata_path(&root, "negative", &forged_build_id, ".neg");
    let distant_expiration = SystemTime::now()
        .checked_add(MAX_FAILURE_TTL)
        .and_then(|expiration| expiration.checked_add(Duration::from_secs(60)))
        .expect("test expiration is representable");
    rewrite_negative_expiration(&path, distant_expiration);
    assert!(matches!(cache.cached_failure(&forged_build_id), Ok(None)));
    assert!(matches!(
        cache.scrub(),
        Ok(ScrubOutcome::Completed(report)) if report.removed_negative == 1
    ));
    assert!(!path.exists());
}

#[test]
fn partial_staging_can_be_replaced_by_a_cached_failure() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(28);
    let mut writer = population(&cache, &build_id)
        .into_writer()
        .expect("population writer is created");
    writer
        .write_all(b"partial GSYM output")
        .expect("partial staging succeeds");

    let failure = writer
        .record_failure_for(FailureKind::ResourceExhausted, Duration::from_secs(30))
        .expect("failure replaces partial output");

    assert_eq!(failure.kind(), FailureKind::ResourceExhausted);
    assert!(!temporary_directory(&root, &build_id).exists());
    assert!(matches!(
        cache.try_begin_population(&build_id),
        Ok(PopulationOutcome::Suppressed(cached))
            if cached.kind() == FailureKind::ResourceExhausted
    ));
}

#[test]
fn malformed_negative_metadata_is_discarded() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(18);
    population(&cache, &build_id)
        .record_failure_for(FailureKind::MissingInput, Duration::from_secs(60))
        .expect("negative entry is recorded");
    let path = metadata_path(&root, "negative", &build_id, ".neg");
    std::fs::set_permissions(&path, {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::Permissions::from_mode(0o600)
    })
    .expect("negative entry is made writable");
    std::fs::write(&path, b"broken").expect("negative entry is corrupted");

    assert!(matches!(cache.cached_failure(&build_id), Ok(None)));
    assert!(path.exists());
    drop(population(&cache, &build_id));
    assert!(!path.exists());
}

#[test]
fn corrupt_positive_entry_is_regenerated() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(20);
    publish(&cache, &build_id, 32);
    let object = object_path(&root, &build_id);
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o600))
        .expect("positive entry is made writable");
    std::fs::write(&object, b"broken").expect("positive entry is corrupted");

    publish(&cache, &build_id, 48);

    assert!(matches!(cache.lookup(&build_id), Ok(Some(_))));
}

#[test]
fn pruning_uses_capacity_hysteresis() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let cache = cache(&directory.path().join("cache"));
    for (byte, name_len) in [(9, 64), (10, 128), (11, 256)] {
        publish(&cache, &identifier(byte), name_len);
    }
    let before = cache.stats().expect("cache statistics are available");
    assert_eq!(before.entries, 3);
    let limit = ByteLimit::new(before.bytes.saturating_sub(1)).expect("cache is nonempty");
    let report = match cache
        .prune(PrunePolicy::new(limit))
        .expect("cache can be pruned")
    {
        PruneOutcome::Completed(report) => report,
        PruneOutcome::Busy => panic!("no other maintenance process is running"),
    };
    assert!(report.removed >= 1);
    assert!(report.after.entries < report.before.entries);
    assert!(report.after.bytes <= limit.get().saturating_mul(4) / 5);
}

#[test]
fn age_only_pruning_removes_entries_below_capacity() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let cache = cache(&directory.path().join("cache"));
    publish(&cache, &identifier(25), 32);
    publish(&cache, &identifier(26), 32);

    let policy = PrunePolicy::new(ByteLimit::new(u64::MAX).expect("maximum byte limit is nonzero"))
        .max_unused_age(Duration::ZERO);
    let mut removed = 0;
    for attempt in 0..100 {
        match cache.prune(policy).expect("cache can be pruned") {
            PruneOutcome::Completed(report) => {
                removed += report.removed;
                if report.after.entries == 0 {
                    assert_eq!(removed, 2, "report: {report:?}");
                    return;
                }
            }
            PruneOutcome::Busy => {}
        }
        if attempt < 99 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    panic!("cache entries remained busy after bounded retries");
}

#[test]
fn scrubbing_removes_corruption_and_old_staging_files() {
    use std::fs::FileTimes;
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary directory is created");
    let root = directory.path().join("cache");
    let cache = cache(&root);
    let build_id = identifier(12);
    publish(&cache, &build_id, 32);
    let object = object_path(&root, &build_id);
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o600))
        .expect("test entry is made writable");
    std::fs::write(&object, b"not a GSYM file").expect("test entry is corrupted");

    let stale_directory = temporary_directory(&root, &identifier(13));
    std::fs::create_dir_all(&stale_directory).expect("orphan staging directory is created");
    let temporary = stale_directory.join("orphan");
    let temporary_file = std::fs::File::create(&temporary).expect("orphan staging file is created");
    temporary_file
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH))
        .expect("orphan staging file is aged");
    drop(temporary_file);

    let future_directory = temporary_directory(&root, &identifier(14));
    std::fs::create_dir_all(&future_directory).expect("future staging directory is created");
    let future_temporary = future_directory.join("orphan");
    let future_file =
        std::fs::File::create(&future_temporary).expect("future-dated staging file is created");
    future_file
        .set_times(
            FileTimes::new().set_modified(
                SystemTime::now()
                    .checked_add(Duration::from_secs(60))
                    .expect("future timestamp is representable"),
            ),
        )
        .expect("orphan staging file is moved into the future");
    drop(future_file);

    let expired_build_id = identifier(15);
    population(&cache, &expired_build_id)
        .record_failure_for(FailureKind::TransientIo, Duration::from_secs(60))
        .expect("negative entry is recorded");
    let expired_negative = metadata_path(&root, "negative", &expired_build_id, ".neg");
    rewrite_negative_expiration(
        &expired_negative,
        UNIX_EPOCH
            .checked_add(Duration::from_secs(1))
            .expect("test expiration is representable"),
    );

    let orphan_access_id = identifier(16);
    assert_eq!(
        cache
            .record_access(&orphan_access_id)
            .expect("orphan access marker is recorded"),
        AccessUpdate::Recorded
    );
    let orphan_access = metadata_path(&root, "access", &orphan_access_id, ".lru");

    let nonregular_object_id = identifier(24);
    assert_eq!(
        cache
            .record_access(&nonregular_object_id)
            .expect("orphan access marker is recorded"),
        AccessUpdate::Recorded
    );
    let nonregular_access = metadata_path(&root, "access", &nonregular_object_id, ".lru");
    let nonregular_object = object_path(&root, &nonregular_object_id);
    std::fs::create_dir_all(&nonregular_object).expect("directory replaces cache object");

    let report = match cache.scrub().expect("cache can be scrubbed") {
        ScrubOutcome::Completed(report) => report,
        ScrubOutcome::Busy => panic!("no other maintenance process is running"),
    };
    assert_eq!(report.checked, 1);
    assert_eq!(report.removed_corrupt, 1);
    assert_eq!(report.removed_temporary, 2, "report: {report:?}");
    assert_eq!(report.removed_negative, 1, "report: {report:?}");
    assert_eq!(report.removed_access, 2, "report: {report:?}");
    assert!(matches!(cache.lookup(&build_id), Ok(None)));
    assert!(!temporary.exists());
    assert!(!future_temporary.exists());
    assert!(!expired_negative.exists());
    assert!(!orphan_access.exists());
    assert!(!nonregular_access.exists());
    assert!(nonregular_object.is_dir());
}
