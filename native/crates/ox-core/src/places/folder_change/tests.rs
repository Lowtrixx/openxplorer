// SPDX-License-Identifier: AGPL-3.0-only
use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    _root: tempfile::TempDir,
    locations: FolderLocations,
    target: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // A real persistent path: /tmp is deliberately rejected by the service.
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let home = root.path().join("home");
        let config = root.path().join("config");
        let target = home.join("Incoming");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(home.join("Downloads")).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(home.join("Downloads/keep.txt"), "existing content").unwrap();
        fs::write(
            config.join("user-dirs.dirs"),
            "# preserved\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n",
        )
        .unwrap();
        Self {
            _root: root,
            locations: FolderLocations::new(home, &config),
            target,
        }
    }
    fn check(&self, value: &Path) -> Result<CheckedFolderChange, String> {
        self.locations.check_change(
            KnownFolder::Downloads,
            value.to_str().unwrap(),
            &WriteProtection::unrestricted(),
        )
    }
}

/// parity: PROP-031
#[test]
fn folder_change_validation_rejects_unsafe_targets_and_resolves_existing_mounts() {
    let f = Fixture::new();
    for path in [
        Path::new("/"),
        Path::new("/tmp"),
        &f.locations.home,
        &f.target.join("missing"),
        &f.locations.home.join("Downloads/keep.txt"),
    ] {
        assert!(f.check(path).is_err(), "{}", path.display());
    }
    assert!(f
        .locations
        .check_change(KnownFolder::Downloads, "", &WriteProtection::unrestricted())
        .is_err());
    assert!(f
        .locations
        .check_change(
            KnownFolder::Downloads,
            "smb://nas.example.invalid/share/Incoming",
            &WriteProtection::unrestricted()
        )
        .is_err());
    let snapshot = f.target.join(".snapshots/1/snapshot");
    fs::create_dir_all(&snapshot).unwrap();
    assert!(f.check(&snapshot).is_err());
    let link = f.locations.home.join("temporary-link");
    symlink("/tmp", &link).unwrap();
    assert!(f.check(&link).is_err());
    let protection = WriteProtection::new(|_| Err(crate::transfer::TransferError::Cancelled));
    assert!(f
        .locations
        .check_change(KnownFolder::Downloads, f.target.to_str().unwrap(), &protection)
        .is_err());
    let readonly = f.target.join("readonly");
    fs::create_dir(&readonly).unwrap();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(f.check(&readonly).is_err());
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o700)).unwrap();
    // Nested dedicated directories are valid: changing settings moves no tree.
    let nested = f.locations.home.join("Downloads/Nested");
    fs::create_dir(&nested).unwrap();
    assert!(f.check(&nested).is_ok());
    let mount = MountEntry {
        path: f.target.to_string_lossy().into(),
        source: "//nas.example.invalid/share".into(),
        root: "/".into(),
        filesystem: "cifs".into(),
        options: "rw".into(),
    };
    let checked = f
        .locations
        .check_with_mounts(
            KnownFolder::Downloads,
            "smb://nas.example.invalid/share",
            &WriteProtection::unrestricted(),
            std::slice::from_ref(&mount),
        )
        .unwrap();
    assert_eq!(checked.path, f.target);
    assert!(checked.network);
    let gvfs = MountEntry {
        filesystem: "fuse.gvfsd-fuse".into(),
        ..mount
    };
    assert!(f
        .locations
        .check_with_mounts(
            KnownFolder::Downloads,
            f.target.to_str().unwrap(),
            &WriteProtection::unrestricted(),
            &[gvfs]
        )
        .is_err());
}

/// parity: PROP-031
#[test]
fn folder_change_requires_confirmation_rechecks_and_preserves_data_and_private_backups() {
    let f = Fixture::new();
    let protection = WriteProtection::unrestricted();
    let checked = f.check(&f.target).unwrap();
    let original = fs::read(f.locations.user_dirs_file()).unwrap();
    let refuses = || -> Result<(), String> { panic!("the updater must not run") };
    assert!(f
        .locations
        .apply_with(&checked, false, &protection, refuses)
        .is_err());
    assert!(!f.locations.state_directory().exists());
    fs::write(
        f.locations.user_dirs_file(),
        "XDG_DOWNLOAD_DIR=\"$HOME/Elsewhere\"\n",
    )
    .unwrap();
    assert!(f
        .locations
        .apply_with(&checked, true, &protection, refuses)
        .is_err());
    fs::write(f.locations.user_dirs_file(), &original).unwrap();
    let link = f.locations.home.join("link");
    symlink(&f.target, &link).unwrap();
    let linked = f.check(&link).unwrap();
    fs::remove_file(&link).unwrap();
    symlink(f.locations.home.join("Downloads"), &link).unwrap();
    assert!(f
        .locations
        .apply_with(&linked, true, &protection, refuses)
        .is_err());
    let same = f.check(&f.locations.home.join("Downloads")).unwrap();
    assert!(f
        .locations
        .apply_with(&same, true, &protection, refuses)
        .unwrap()
        .backup
        .is_none());
    assert!(!f.locations.state_directory().exists());
    let failed = f
        .locations
        .apply_with(&checked, true, &protection, || {
            Err("simulated command failure".into())
        })
        .unwrap_err();
    assert!(failed.contains("Configuration backup:"));
    assert_eq!(fs::read(f.locations.user_dirs_file()).unwrap(), original);
    assert!(f
        .locations
        .apply_with(&checked, true, &protection, || Ok(()))
        .unwrap_err()
        .contains("did not retain"));
    // Exercise the actual installed xdg utility with explicit disposable HOME/config.
    let guard = FolderChangeGuard::acquire().unwrap();
    assert!(FolderChangeGuard::acquire().is_err());
    let outcome = f
        .locations
        .apply_change(&checked, true, &protection, &guard)
        .unwrap();
    drop(guard);
    assert!(!FolderChangeGuard::is_busy());
    let backup = outcome.backup.unwrap();
    assert_eq!(fs::read(&backup).unwrap(), original);
    assert_eq!(fs::metadata(&backup).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(backup.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        f.locations.current_path(KnownFolder::Downloads).unwrap(),
        f.target
    );
    assert_eq!(
        f.locations.previous_path(KnownFolder::Downloads).unwrap(),
        checked.previous
    );
    assert_eq!(
        fs::read_to_string(checked.previous.join("keep.txt")).unwrap(),
        "existing content"
    );
    assert!(!f.target.join("keep.txt").exists());
    assert!(outcome.warning.is_none());
}

/// parity: PROP-031
#[test]
fn folder_change_reports_partial_failure_and_history_failure_accurately() {
    let f = Fixture::new();
    let protection = WriteProtection::unrestricted();
    let checked = f.check(&f.target).unwrap();
    let original = fs::read(f.locations.user_dirs_file()).unwrap();
    let changed = format!("XDG_DOWNLOAD_DIR=\"{}\"\n", f.target.display());
    let error = f
        .locations
        .apply_with(&checked, true, &protection, || {
            fs::write(f.locations.user_dirs_file(), &changed).unwrap();
            Err("simulated failure after writing".into())
        })
        .unwrap_err();
    assert!(error.contains("Configuration backup:"));
    assert!(error.contains("Check the current location"));
    assert_eq!(
        f.locations.current_path(KnownFolder::Downloads).unwrap(),
        f.target
    );
    fs::write(f.locations.user_dirs_file(), original).unwrap();
    fs::create_dir(f.locations.state_directory().join("folder-location-history.json")).unwrap();
    let outcome = f
        .locations
        .apply_with(&checked, true, &protection, || {
            fs::write(f.locations.user_dirs_file(), &changed).unwrap();
            Ok(())
        })
        .unwrap();
    assert!(outcome.backup.unwrap().is_file());
    assert!(outcome.warning.unwrap().contains("location changed"));
    assert_eq!(
        f.locations.current_path(KnownFolder::Downloads).unwrap(),
        f.target
    );
}
