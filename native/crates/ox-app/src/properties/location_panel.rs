// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit check and confirmation for an XDG standard-folder change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::{gio, glib, prelude::*};
use ox_core::ops::WriteProtection;
use ox_core::places::{CheckedFolderChange, FolderChangeGuard, FolderLocations, KnownFolder};

use crate::dialog_layer::quiet_text;

pub(super) struct LocationPanel {
    pub(super) widget: gtk::Box,
    entry: gtk::Entry,
    current: gtk::Label,
    status: gtk::Label,
    consent: gtk::CheckButton,
    apply: gtk::Button,
    check: gtk::Button,
    previous: gtk::Button,
    folder: KnownFolder,
    locations: FolderLocations,
    protection: WriteProtection,
    checked: RefCell<Option<CheckedFolderChange>>,
    generation: Cell<u64>,
    busy: Cell<bool>,
    closed: Cell<bool>,
    can_apply: Rc<dyn Fn() -> bool>,
    changed: Rc<dyn Fn()>,
}

impl std::fmt::Debug for LocationPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocationPanel")
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

impl LocationPanel {
    pub(super) fn new(
        folder: KnownFolder,
        locations: FolderLocations,
        protection: WriteProtection,
        can_apply: Rc<dyn Fn() -> bool>,
        changed: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 10);
        widget.append(&quiet_text(&format!("Choose where {} is stored. Applications that honor Linux's standard-folder settings will use this location.", folder.label())));
        let current = quiet_text("Reading current location…");
        current.set_selectable(true);
        widget.append(&current);
        let entry = gtk::Entry::builder().hexpand(true).build();
        let label = gtk::Label::builder()
            .label("_Folder location")
            .use_underline(true)
            .xalign(0.0)
            .mnemonic_widget(&entry)
            .build();
        widget.append(&label);
        widget.append(&entry);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let browse = gtk::Button::with_label("Browse…");
        let check = gtk::Button::with_label("Check location");
        let default = gtk::Button::with_label("Restore default");
        let previous = gtk::Button::with_label("Use previous");
        for button in [&browse, &check, &default, &previous] {
            row.append(button);
        }
        widget.append(&row);
        let status = quiet_text("");
        status.set_selectable(true);
        widget.append(&status);
        widget.append(&quiet_text("Existing files will NOT be moved. Use an existing local folder or a persistent mounted network folder. Applications may need to be restarted."));
        let consent =
            gtk::CheckButton::with_label("Change the system folder location; leave existing files in place.");
        widget.append(&consent);
        let apply = gtk::Button::builder()
            .label("Apply location")
            .halign(gtk::Align::Start)
            .sensitive(false)
            .build();
        widget.append(&apply);
        let panel = Rc::new(Self {
            widget,
            entry,
            current,
            status,
            consent,
            apply,
            check,
            previous,
            folder,
            locations,
            protection,
            checked: RefCell::new(None),
            generation: Cell::new(0),
            busy: Cell::new(false),
            closed: Cell::new(false),
            can_apply,
            changed,
        });
        panel.connect_controls(&browse, &default);
        panel.load();
        panel
    }

    fn connect_controls(self: &Rc<Self>, browse: &gtk::Button, default: &gtk::Button) {
        let panel = self;
        let folder = self.folder;
        let weak = Rc::downgrade(panel);
        panel.entry.connect_changed(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.invalidate();
            }
        });
        let weak = Rc::downgrade(panel);
        panel.consent.connect_toggled(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.update_apply();
            }
        });
        let weak = Rc::downgrade(panel);
        panel.check.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.check_location();
            }
        });
        let weak = Rc::downgrade(panel);
        panel.apply.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.apply_location();
            }
        });
        let weak = Rc::downgrade(panel);
        default.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel
                    .entry
                    .set_text(&panel.locations.default_paths().path(folder).to_string_lossy());
            }
        });
        let weak = Rc::downgrade(panel);
        panel.previous.connect_clicked(move |_| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            let locations = panel.locations.clone();
            let weak = Rc::downgrade(&panel);
            glib::spawn_future_local(async move {
                let previous = gio::spawn_blocking(move || locations.previous_path(folder)).await;
                if let (Some(panel), Ok(Some(path))) = (weak.upgrade(), previous) {
                    if !panel.closed.get() && !panel.busy.get() {
                        panel.entry.set_text(&path.to_string_lossy());
                    }
                }
            });
        });
        let weak = Rc::downgrade(panel);
        browse.connect_clicked(move |_| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            let parent = panel.widget.root().and_downcast::<gtk::Window>();
            let chooser = gtk::FileDialog::builder()
                .title("Choose an existing folder")
                .build();
            let weak = Rc::downgrade(&panel);
            glib::spawn_future_local(async move {
                if let Ok(file) = chooser.select_folder_future(parent.as_ref()).await {
                    if let (Some(panel), Some(path)) = (weak.upgrade(), file.path()) {
                        if !panel.closed.get() && !panel.busy.get() {
                            panel.entry.set_text(&path.to_string_lossy());
                        }
                    }
                }
            });
        });
    }

    fn load(self: &Rc<Self>) {
        let locations = self.locations.clone();
        let folder = self.folder;
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::spawn_future_local(async move {
            let reading = gio::spawn_blocking(move || {
                (locations.current_path(folder), locations.previous_path(folder))
            })
            .await;
            let Some(panel) = weak.upgrade().filter(|p| !p.closed.get()) else {
                return;
            };
            if let Ok((current, previous)) = reading {
                panel.previous.set_visible(previous.is_some());
                match current {
                    Ok(path) => {
                        panel
                            .current
                            .set_text(&format!("Current location: {}", path.display()));
                        if panel.generation.get() == generation {
                            panel.entry.set_text(&path.to_string_lossy());
                        }
                    }
                    Err(error) => panel.status.set_text(&error),
                }
            }
        });
    }

    fn invalidate(&self) {
        self.generation.set(self.generation.get() + 1);
        self.checked.replace(None);
        self.consent.set_active(false);
        self.update_apply();
    }

    fn update_apply(&self) {
        self.apply.set_sensitive(
            !self.closed.get()
                && !self.busy.get()
                && self.consent.is_active()
                && self.checked.borrow().is_some(),
        );
    }

    fn check_location(self: &Rc<Self>) {
        if self.closed.get() || self.busy.replace(true) {
            return;
        }
        self.invalidate();
        self.check.set_sensitive(false);
        self.status.set_text("Checking location…");
        let generation = self.generation.get();
        let locations = self.locations.clone();
        let folder = self.folder;
        let input = self.entry.text().to_string();
        let protection = self.protection.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result =
                gio::spawn_blocking(move || locations.check_change(folder, &input, &protection)).await;
            let Some(panel) = weak.upgrade().filter(|p| !p.closed.get()) else {
                return;
            };
            panel.busy.set(false);
            panel.check.set_sensitive(true);
            if panel.generation.get() != generation {
                panel.status.set_text("The destination changed. Check it again.");
                return;
            }
            match result {
                Ok(Ok(checked)) => {
                    let kind = if checked.network {
                        "Mounted network folder"
                    } else {
                        "Local folder"
                    };
                    panel
                        .status
                        .set_text(&format!("{kind} · {}", checked.path.display()));
                    panel.checked.replace(Some(checked));
                }
                Ok(Err(error)) => panel.status.set_text(&error),
                Err(_) => panel.status.set_text("The location check could not finish."),
            }
            panel.update_apply();
        });
    }

    fn apply_location(self: &Rc<Self>) {
        if self.closed.get() || self.busy.get() || !self.consent.is_active() {
            return;
        }
        let Some(checked) = self.checked.borrow().clone() else {
            return;
        };
        if !(self.can_apply)() {
            self.status
                .set_text("Wait for file operations in all windows to finish, then apply again.");
            return;
        }
        let guard = match FolderChangeGuard::acquire() {
            Ok(guard) => guard,
            Err(error) => {
                self.status.set_text(&error);
                return;
            }
        };
        self.busy.set(true);
        self.widget.set_sensitive(false);
        self.status.set_text("Applying location…");
        let locations = self.locations.clone();
        let protection = self.protection.clone();
        let folder = self.folder;
        let changed = self.changed.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let result = locations.apply_change(&checked, true, &protection, &guard);
                let current = locations.current_path(folder);
                (result, current)
            })
            .await;
            changed();
            let Some(panel) = weak.upgrade().filter(|p| !p.closed.get()) else {
                return;
            };
            panel.busy.set(false);
            panel.widget.set_sensitive(true);
            panel.invalidate();
            match result {
                Ok((result, current)) => {
                    if let Ok(path) = current {
                        panel
                            .current
                            .set_text(&format!("Current location: {}", path.display()));
                    }
                    match result {
                        Ok(outcome) => {
                            let message = outcome.backup.map_or_else(
                                || "This is already the current location. No changes made.".to_owned(),
                                |backup| {
                                    format!(
                                        "Location updated. Configuration backed up to {}. No files moved.",
                                        backup.display()
                                    )
                                },
                            );
                            panel.status.set_text(
                                &outcome
                                    .warning
                                    .map_or(message.clone(), |warning| format!("{message}\n{warning}")),
                            );
                            panel.load();
                        }
                        Err(error) => panel.status.set_text(&error),
                    }
                }
                Err(_) => panel.status.set_text(
                    "The location update could not finish. Check the current setting before retrying.",
                ),
            }
        });
    }

    pub(super) fn cancel(&self) {
        self.closed.set(true);
        self.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::harness::wait_until;
    use std::fs;

    #[gtk::test]
    fn location_panel_checks_consent_edits_busy_apply_and_close() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let home = root.path().join("home");
        let config = root.path().join("config");
        let target = home.join("Incoming");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(home.join("Downloads")).unwrap();
        fs::create_dir(&config).unwrap();
        let original = "XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n";
        fs::write(config.join("user-dirs.dirs"), original).unwrap();
        fs::write(home.join("Downloads/keep.txt"), "keep").unwrap();
        let locations = FolderLocations::new(home.clone(), &config);
        let idle = Rc::new(Cell::new(false));
        let idle_test = idle.clone();
        let refreshes = Rc::new(Cell::new(0));
        let refresh_test = refreshes.clone();
        let panel = LocationPanel::new(
            KnownFolder::Downloads,
            locations.clone(),
            WriteProtection::unrestricted(),
            Rc::new(move || idle_test.get()),
            Rc::new(move || refresh_test.set(refresh_test.get() + 1)),
        );
        wait_until("initial location", || !panel.entry.text().is_empty());
        assert!(!panel.consent.is_active());
        assert!(!panel.apply.is_sensitive());
        panel.entry.set_text("/tmp");
        panel.check.emit_clicked();
        wait_until("invalid check", || !panel.busy.get());
        assert!(panel.checked.borrow().is_none());
        panel.consent.set_active(true);
        assert!(!panel.apply.is_sensitive());
        panel.apply.emit_clicked();
        assert_eq!(fs::read_to_string(locations.user_dirs_file()).unwrap(), original);
        panel.entry.set_text(target.to_str().unwrap());
        panel.check.emit_clicked();
        panel.entry.set_text(home.join("Downloads").to_str().unwrap());
        wait_until("edited pending check", || !panel.busy.get());
        assert!(panel.status.text().contains("changed"));
        assert!(!panel.apply.is_sensitive());
        panel.entry.set_text(target.to_str().unwrap());
        panel.check.emit_clicked();
        wait_until("valid check", || !panel.busy.get());
        assert!(panel.checked.borrow().is_some());
        assert!(!panel.apply.is_sensitive());
        panel.consent.set_active(true);
        assert!(panel.apply.is_sensitive());
        panel.apply.emit_clicked();
        assert!(panel.status.text().contains("file operations"));
        assert_eq!(fs::read_to_string(locations.user_dirs_file()).unwrap(), original);
        idle.set(true);
        panel.apply.emit_clicked();
        assert!(!panel.widget.is_sensitive());
        wait_until("applied", || !panel.busy.get());
        assert!(
            panel.status.text().contains("Location updated"),
            "{}",
            panel.status.text()
        );
        assert_eq!(locations.current_path(KnownFolder::Downloads).unwrap(), target);
        assert_eq!(refreshes.get(), 1);
        assert_eq!(
            fs::read_to_string(home.join("Downloads/keep.txt")).unwrap(),
            "keep"
        );
        assert!(!target.join("keep.txt").exists());
        wait_until("previous button", || panel.previous.is_visible());
        panel.previous.emit_clicked();
        wait_until("previous field", || {
            panel.entry.text().as_str() == home.join("Downloads").to_str().unwrap()
        });
        assert!(!panel.consent.is_active());
        assert!(!panel.apply.is_sensitive());
        panel.check.emit_clicked();
        panel.cancel();
        let weak = Rc::downgrade(&panel);
        drop(panel);
        assert!(
            weak.upgrade().is_none(),
            "signals and pending reads do not retain a closed panel"
        );
        assert!(!FolderChangeGuard::is_busy());
    }
}
