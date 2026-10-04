# Downloads & Brave

A familiar Location tab, with Linux filesystem semantics.

## Set a standard folder location

Right-click Downloads or Documents → Properties → Location. Choose an existing writable directory and click Apply. Validation runs automatically before the change. The previous user-directory setting is backed up. No existing files are moved, merged or deleted.

The native Location tab needs xdg-user-dirs and supports existing local folders and persistent mounted SMB destinations. Flatpak cannot change host standard-folder settings. The native tab does not include the mount setup assistant or the Brave follow-up; those remain separate settings and tools.

## Group files by modification date

The native app groups your standard Downloads folder by modification date, including when its location has changed. The Details view shows headings for Today, Yesterday, This week, Last week, This month, Last month and Long time ago, using local calendar dates. Future dates and unknown dates appear separately. Icon views retain the grouped order without headings.

Use Sort → Group by date modified or No grouping in any folder. Automatic (Downloads) restores the default. The choice stays with the open tab; the selected file sort still applies within each group.

## Use a stable mount for a network destination

Use a persistent Linux mount path such as /mnt/nas/downloads. Temporary per-login GVfs paths and an unmounted SMB URL are not suitable replacements for a standard folder.

> An offline NAS cannot accept a download. The search index is not an offline synchronization engine.

## Optionally sync native Brave profiles

Also update Brave’s download directory opens a separate profile-selection and confirmation step. Fully quit Brave, including background processes, first. Only download and Save As directory fields are changed, with private backups and field-level restoration.

## Sandboxed or custom browser profiles

For unsupported Flatpak/Snap, custom or policy-managed profiles, set the same mounted path manually in Brave. Filesystem access permissions may also need review.

```sh
brave://settings/downloads
```

## Review the optional mount helper

The optional helper prepares persistent systemd mount/automount configuration. It needs administrator approval and uses a root-readable plaintext CIFS credential file, separate from the desktop keyring. It is not invoked during package installation. See desktop/ZORIN-SETUP.md before using it.

---

OpenXplorer 2.0.0. Project-authored documentation: AGPL-3.0-only.
