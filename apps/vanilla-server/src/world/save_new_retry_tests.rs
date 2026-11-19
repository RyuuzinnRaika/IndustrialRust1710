//! Synthetic create-only world publication regressions. No existing world is used.
use super::*;

fn fixture(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ir1710-new-world-retry-{label}-{}-{stamp}",
        std::process::id()
    ));
    // Exclusive creation: an existing directory is never reused or removed.
    fs::create_dir(&root).unwrap();
    let staging = root.join("world.importing");
    let destination = root.join("world.irw");
    (root, staging, destination)
}

#[test]
fn interrupted_link_retries_the_same_staging_then_stops_on_success() {
    let (root, staging, destination) = fixture("success");
    let bytes = b"synthetic fully staged image";
    fs::write(&staging, bytes).unwrap();
    let mut attempts = 0;
    publish_new_world(&staging, &destination, |source, target| {
        attempts += 1;
        assert_eq!(source, staging);
        assert_eq!(target, destination);
        assert_eq!(fs::read(source).unwrap(), bytes);
        assert!(!target.exists());
        if attempts < 3 {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            fs::hard_link(source, target)
        }
    })
    .unwrap();
    assert_eq!(
        attempts, 3,
        "no attempt may follow a successful publication"
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(!staging.exists());
    fs::remove_file(&destination).unwrap();
    fs::remove_dir(&root).unwrap();
}

#[test]
fn competing_destination_stops_retry_without_overwriting_it() {
    let (root, staging, destination) = fixture("competitor");
    fs::write(&staging, b"owned staged bytes").unwrap();
    let competing = b"independent winner";
    let mut attempts = 0;
    let error = publish_new_world(&staging, &destination, |source, target| {
        attempts += 1;
        assert_eq!(fs::read(source).unwrap(), b"owned staged bytes");
        if attempts == 1 {
            // Model another writer winning after the first interrupted attempt.
            fs::write(target, competing).unwrap();
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            fs::hard_link(source, target)
        }
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(attempts, 2);
    assert_eq!(fs::read(&destination).unwrap(), competing);
    assert!(!staging.exists());
    fs::remove_file(&destination).unwrap();
    fs::remove_dir(&root).unwrap();
}

#[test]
fn ambiguous_prior_link_is_not_reinterpreted_as_success_or_removed() {
    let (root, staging, destination) = fixture("ambiguous");
    let bytes = b"synthetic linked bytes";
    fs::write(&staging, bytes).unwrap();
    let mut attempts = 0;
    let error = publish_new_world(&staging, &destination, |source, target| {
        attempts += 1;
        if attempts == 1 {
            // Model an error reported after the destination was actually created.
            fs::hard_link(source, target).unwrap();
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            fs::hard_link(source, target)
        }
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(attempts, 2);
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(!staging.exists());
    fs::remove_file(&destination).unwrap();
    fs::remove_dir(&root).unwrap();
}

#[test]
fn non_retryable_error_keeps_other_destination_and_cleans_owned_staging() {
    let (root, staging, destination) = fixture("permanent");
    fs::write(&staging, b"owned staged bytes").unwrap();
    fs::write(&destination, b"existing independent bytes").unwrap();
    let mut attempts = 0;
    let error = publish_new_world(&staging, &destination, |source, _| {
        attempts += 1;
        assert_eq!(fs::read(source).unwrap(), b"owned staged bytes");
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "synthetic denial",
        ))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(attempts, 1);
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"existing independent bytes"
    );
    assert!(!staging.exists());
    fs::remove_file(&destination).unwrap();
    fs::remove_dir(&root).unwrap();
}

#[cfg(windows)]
#[test]
fn injected_windows_denials_then_real_link_keep_the_staged_image() {
    let (root, staging, destination) = fixture("windows-recovers");
    let bytes = b"synthetic fully staged Windows image";
    fs::write(&staging, bytes).unwrap();
    let mut attempts = 0;
    publish_new_world(&staging, &destination, |source, target| {
        attempts += 1;
        assert_eq!(source, staging);
        assert_eq!(target, destination);
        assert_eq!(fs::read(source).unwrap(), bytes);
        assert!(!target.exists());
        match attempts {
            1 => Err(io::Error::from_raw_os_error(5)),
            2 => Err(io::Error::from_raw_os_error(32)),
            _ => fs::hard_link(source, target),
        }
    })
    .unwrap();
    assert_eq!(
        attempts, 3,
        "no attempt may follow a successful publication"
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(!staging.exists());
    fs::remove_file(&destination).unwrap();
    fs::remove_dir(&root).unwrap();
}

#[cfg(windows)]
#[test]
fn exhausted_windows_link_retries_keep_last_raw_os_error() {
    let (root, staging, destination) = fixture("windows-exhausted");
    fs::write(&staging, b"owned staged bytes").unwrap();
    let mut attempts = 0;
    let error = publish_new_world(&staging, &destination, |source, target| {
        attempts += 1;
        assert_eq!(fs::read(source).unwrap(), b"owned staged bytes");
        assert!(!target.exists());
        Err(io::Error::from_raw_os_error(if attempts < 6 {
            5
        } else {
            32
        }))
    })
    .unwrap_err();
    assert_eq!(attempts, 6);
    assert_eq!(error.raw_os_error(), None, "context wraps the OS error");
    let context = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<NewWorldPublishFailure>())
        .expect("new-world publish context");
    assert_eq!(context.source.raw_os_error(), Some(32));
    let source = std::error::Error::source(context)
        .and_then(|source| source.downcast_ref::<io::Error>())
        .expect("final OS error as source");
    assert_eq!(source.raw_os_error(), Some(32));
    assert!(!destination.exists());
    assert!(!staging.exists());
    fs::remove_dir(&root).unwrap();
}

#[test]
fn existing_staging_is_not_owned_or_deleted_by_save_new() {
    let (root, staging, destination) = fixture("staging-owned");
    fs::write(&staging, b"pre-existing unrelated staging").unwrap();
    let error = World::new().save_new(&destination).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        fs::read(&staging).unwrap(),
        b"pre-existing unrelated staging"
    );
    assert!(!destination.exists());
    fs::remove_file(&staging).unwrap();
    fs::remove_dir(&root).unwrap();
}
