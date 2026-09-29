// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit XDG folder changes. Existing contents are never moved.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gio::prelude::*;
use rustix::fs::{access, Access, OFlags};

use super::{user_dirs, FolderLocations, KnownFolder};
use crate::integration::{host_command::HostCommand, Sandbox};
use crate::location::{file_uri, normalise_location};
use crate::network::{mount_for_path, read_mount_table, resolve_smb_path, MountEntry};
use crate::ops::WriteProtection;
use crate::private_storage::{private_directory, replace_file_atomically, KernelOpenFlags};
use crate::versions::is_conventional_snapshot;

// ponytail: serialize XDG changes process-wide; use a per-config lock if multiple profiles are added.
static CHANGING: AtomicBool = AtomicBool::new(false);

/// Reserves a location change before starting its worker. Drop releases it,
/// even if the dialog closes or the worker fails.
#[derive(Debug)]
pub struct FolderChangeGuard(());

impl FolderChangeGuard {
    /// Whether a location change is running in any window.
    pub fn is_busy() -> bool {
        CHANGING.load(Ordering::Acquire)
    }

    /// Reserve the change, or refuse while another change runs.
    ///
    /// # Errors
    /// Returns the busy explanation when already reserved.
    pub fn acquire() -> Result<Self, String> {
        CHANGING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(()))
            .map_err(|_| "Another folder location change is running.".to_owned())
    }
}

impl Drop for FolderChangeGuard {
    fn drop(&mut self) {
        CHANGING.store(false, Ordering::Release);
    }
}

/// A checked destination and the configuration the user confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedFolderChange {
    folder: KnownFolder,
    input: String,
    identity: (u64, u64),
    mount_source: Option<(String, String)>,
    /// Canonical, existing destination.
    pub path: PathBuf,
    /// Current configured location when checked.
    pub previous: PathBuf,
    /// Whether this is an existing kernel CIFS/SMB3 mount.
    pub network: bool,
}

/// Successful change, including a history-write warning if applicable.
#[derive(Debug)]
pub struct FolderChangeOutcome {
    /// Backup of the old configuration, absent for an unchanged location.
    pub backup: Option<PathBuf>,
    /// A history failure does not disguise a successful XDG change.
    pub warning: Option<String>,
}

impl FolderLocations {
    /// Read one current path without hiding malformed or unreadable config.
    ///
    /// # Errors
    /// Returns the configuration read error.
    pub fn current_path(&self, folder: KnownFolder) -> Result<PathBuf, String> {
        let configured = user_dirs::read(&self.user_dirs_file, &self.home).map_err(|e| e.to_string())?;
        Ok(self.paths_with(configured).path(folder).to_owned())
    }

    /// Last successful change's previous path, for the Use previous button.
    pub fn previous_path(&self, folder: KnownFolder) -> Option<PathBuf> {
        self.history()
            .get(folder.xdg_key())?
            .get("previous")?
            .as_str()
            .map(PathBuf::from)
    }

    fn config_directory(&self) -> &Path {
        self.user_dirs_file
            .parent()
            .expect("FolderLocations::new appends the filename")
    }

    fn state_directory(&self) -> PathBuf {
        self.config_directory().join("winspace")
    }

    fn history(&self) -> serde_json::Value {
        let path = self.state_directory().join("folder-location-history.json");
        read_change_file(&path)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}))
    }

    /// Check a local folder or an SMB URI backed by an existing kernel mount.
    /// Does not create folders or write configuration.
    ///
    /// # Errors
    /// Refuses temporary, protected, unwritable and unavailable destinations.
    pub fn check_change(
        &self,
        folder: KnownFolder,
        value: &str,
        protection: &WriteProtection,
    ) -> Result<CheckedFolderChange, String> {
        if Sandbox::detect().is_flatpak() {
            return Err("Changing host standard folders is unavailable inside Flatpak. Use the host's folder settings.".into());
        }
        let mounts = read_mount_table().map_err(|e| e.to_string())?;
        self.check_with_mounts(folder, value, protection, &mounts)
    }

    fn check_with_mounts(
        &self,
        folder: KnownFolder,
        value: &str,
        protection: &WriteProtection,
        mounts: &[MountEntry],
    ) -> Result<CheckedFolderChange, String> {
        if value.trim().is_empty() {
            return Err("Choose an existing folder.".into());
        }
        let uri = normalise_location(value, None, &self.home).map_err(|e| e.to_string())?;
        let target = if uri.starts_with("smb:") {
            resolve_smb_path(&uri, mounts).map_err(|e| e.to_string())?.ok_or(
                "This SMB folder needs an existing persistent CIFS mount. A sidebar bookmark is not enough.",
            )?
        } else {
            gio::File::for_uri(&uri)
                .path()
                .ok_or("Use a local path or an existing mounted SMB folder.")?
        };
        let real = target
            .canonicalize()
            .map_err(|e| format!("The new location must be an existing folder: {e}"))?;
        if ["/run", "/tmp", "/var/tmp"]
            .iter()
            .any(|base| real.starts_with(base))
        {
            return Err("Use a persistent location, not a temporary or per-login GVfs path.".into());
        }
        let mount = mount_for_path(&real.to_string_lossy(), mounts);
        if mount.is_some_and(|m| m.filesystem.contains("gvfs")) {
            return Err("GVfs session paths cannot be used as persistent standard folders.".into());
        }
        if !real.is_dir() {
            return Err("The new location must be an existing folder.".into());
        }
        if real == Path::new("/") || real == self.home.canonicalize().map_err(|e| e.to_string())? {
            return Err(
                "Choose a dedicated folder, not your entire home directory or the filesystem root.".into(),
            );
        }
        let mut protected_uris = vec![uri, file_uri(&real)];
        if let Some(mount) = mount {
            if let (Some(remote), Ok(relative)) = (mount.remote_root(), real.strip_prefix(&mount.path)) {
                protected_uris.push(
                    gio::File::for_uri(&remote)
                        .resolve_relative_path(relative)
                        .uri()
                        .to_string(),
                );
            }
        }
        for location in &protected_uris {
            if is_conventional_snapshot(location) {
                return Err("Snapshot folders are read-only and cannot be standard folder locations.".into());
            }
            protection.check(location).map_err(|e| e.to_string())?;
        }
        access(&real, Access::WRITE_OK | Access::EXEC_OK)
            .map_err(|_| "You do not have write access to this folder.")?;
        if real.to_str().is_none() {
            return Err("The folder path must be valid UTF-8.".into());
        }
        let metadata = real.metadata().map_err(|e| e.to_string())?;
        Ok(CheckedFolderChange {
            folder,
            input: value.to_owned(),
            identity: (metadata.dev(), metadata.ino()),
            mount_source: mount.map(|m| (m.source.clone(), m.root.clone())),
            path: real,
            previous: self.current_path(folder)?,
            network: mount.is_some_and(MountEntry::is_smb),
        })
    }

    /// Apply an explicitly confirmed check, retaining a private backup.
    /// Rechecks the destination and current setting before any write.
    /// The caller holds `guard` until completion and blocks file operations.
    ///
    /// # Errors
    /// Refuses missing consent, stale checks or failed updates. Errors after
    /// the backup include its path; no file contents are ever moved.
    pub fn apply_change(
        &self,
        checked: &CheckedFolderChange,
        confirmed: bool,
        protection: &WriteProtection,
        _guard: &FolderChangeGuard,
    ) -> Result<FolderChangeOutcome, String> {
        self.apply_with(checked, confirmed, protection, || {
            HostCommand::new("xdg-user-dirs-update")
                .arg("--set")
                .arg(checked.folder.xdg_key())
                .arg(&checked.path)
                .env("HOME", &self.home)
                .env("XDG_CONFIG_HOME", self.config_directory())
                .output_within(Sandbox::Host, Duration::from_secs(15))
                .map(|_| ())
                .map_err(|e| format!("xdg-user-dirs-update: {e}. Install xdg-user-dirs if it is missing."))
        })
    }

    fn apply_with(
        &self,
        checked: &CheckedFolderChange,
        confirmed: bool,
        protection: &WriteProtection,
        run: impl FnOnce() -> Result<(), String>,
    ) -> Result<FolderChangeOutcome, String> {
        if !confirmed {
            return Err("Confirm the new location before applying it.".into());
        }
        let fresh = self.check_change(checked.folder, &checked.input, protection)?;
        if fresh != *checked {
            return Err("The destination or current setting changed. Check it again.".into());
        }
        if checked.path == checked.previous {
            return Ok(FolderChangeOutcome {
                backup: None,
                warning: None,
            });
        }
        let old = read_change_file(&self.user_dirs_file)?.unwrap_or_default();
        let directory = self.state_directory();
        private_directory(&directory).map_err(|e| e.to_string())?;
        let backups = directory.join("location-backups");
        private_directory(&backups).map_err(|e| e.to_string())?;
        let mut backup = tempfile::Builder::new()
            .prefix("user-dirs-")
            .suffix(".dirs")
            .tempfile_in(&backups)
            .map_err(|e| e.to_string())?;
        backup
            .write_all(&old)
            .and_then(|()| backup.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        let (_, backup) = backup.keep().map_err(|e| e.to_string())?;
        let failure = |message: String| {
            format!("{message} Configuration backup: {}. No user files were moved. Check the current location before retrying.", backup.display())
        };
        run().map_err(&failure)?;
        if self.current_path(checked.folder).map_err(&failure)? != checked.path {
            return Err(failure(
                "The folder configuration did not retain the requested path.".into(),
            ));
        }
        let mut history = self.history();
        history[checked.folder.xdg_key()] = serde_json::json!({
            "previous": checked.previous, "path": checked.path, "backup": backup,
            "changedAt": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
        });
        let warning = replace_file_atomically(
            &directory.join("folder-location-history.json"),
            ".locations-",
            &serde_json::to_vec_pretty(&history).expect("JSON values serialize"),
        )
        .err()
        .map(|e| format!("The location changed, but its history could not be saved: {e}"));
        Ok(FolderChangeOutcome {
            backup: Some(backup),
            warning,
        })
    }
}

/// Bounded, nonblocking reads for the config backup and private history.
fn read_change_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .kernel_flags(OFlags::NOFOLLOW | OFlags::NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > 128 * 1024 {
        return Err(format!(
            "{} must be a regular file under 128 KiB, not a link or device.",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 128 * 1024 {
        return Err("The configuration is unexpectedly large.".into());
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;
