// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit confirmation for an XDG standard-folder change.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::{gio, glib, prelude::*};
use ox_core::ops::WriteProtection;
use ox_core::places::{FolderChangeGuard, FolderLocations, KnownFolder};

use crate::dialog_layer::quiet_text;
use crate::window::ButtonStyle;

pub(super) struct LocationPanel {
    pub(super) widget: gtk::Box,
    entry: gtk::Entry,
    current: gtk::Label,
    status: gtk::Label,
    previous: gtk::Button,
    folder: KnownFolder,
    locations: FolderLocations,
    protection: WriteProtection,
    displayed_current: RefCell<Option<PathBuf>>,
    generation: Cell<u64>,
    loading: Cell<bool>,
    busy: Cell<bool>,
    closed: Cell<bool>,
    apply: RefCell<Option<gtk::Button>>,
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
        widget.append(&quiet_text(&format!(
            "Choose the folder applications use for {}.",
            folder.label()
        )));
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
        let default = gtk::Button::with_label("Restore default");
        let previous = gtk::Button::with_label("Use previous");
        for button in [&browse, &default, &previous] {
            button.add_css_class(ButtonStyle::Bordered.css_class());
            row.append(button);
        }
        widget.append(&row);
        let status = quiet_text("");
        status.set_selectable(true);
        widget.append(&status);
        widget.append(&quiet_text("Existing files stay in their current location."));
        let panel = Rc::new(Self {
            widget,
            entry,
            current,
            status,
            previous,
            folder,
            locations,
            protection,
            displayed_current: RefCell::new(None),
            generation: Cell::new(0),
            loading: Cell::new(false),
            busy: Cell::new(false),
            closed: Cell::new(false),
            apply: RefCell::new(None),
            can_apply,
            changed,
        });
        panel.connect_controls(&browse, &default);
        panel.load();
        panel
    }

    pub(super) fn attach_apply(self: &Rc<Self>, button: &gtk::Button) {
        self.apply.replace(Some(button.clone()));
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.apply_location();
            }
        });
        self.update_apply();
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
        self.loading.set(true);
        self.update_apply();
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
            panel.loading.set(false);
            if let Ok((current, previous)) = reading {
                panel.previous.set_visible(previous.is_some());
                match current {
                    Ok(path) => {
                        let had_current = panel.displayed_current.borrow().is_some();
                        if !had_current || panel.generation.get() == generation {
                            panel.displayed_current.replace(Some(path.clone()));
                        }
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
            panel.update_apply();
        });
    }

    fn invalidate(&self) {
        self.generation.set(self.generation.get() + 1);
        self.update_apply();
    }

    fn update_apply(&self) {
        let input = self.entry.text();
        let unchanged = self
            .displayed_current
            .borrow()
            .as_ref()
            .is_some_and(|current| Path::new(input.as_str()) == current);
        let sensitive = !self.closed.get()
            && !self.loading.get()
            && !self.busy.get()
            && !input.trim().is_empty()
            && !unchanged;
        if let Some(button) = self.apply.borrow().as_ref() {
            button.set_sensitive(sensitive);
        }
    }

    fn apply_location(self: &Rc<Self>) {
        if self.closed.get() || self.loading.get() || self.busy.get() {
            return;
        }
        let input = self.entry.text().to_string();
        if input.trim().is_empty() {
            return;
        }
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
        self.update_apply();
        let locations = self.locations.clone();
        let protection = self.protection.clone();
        let folder = self.folder;
        let displayed_current = self.displayed_current.borrow().clone();
        let changed = self.changed.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let checked = locations.check_change(folder, &input, &protection)?;
                if displayed_current.as_ref() != Some(&checked.previous) {
                    return Err("The current location changed. Review it and apply again.".to_owned());
                }
                let result = locations.apply_change(&checked, true, &protection, &guard);
                let current = locations.current_path(folder);
                Ok((result, current))
            })
            .await;
            if matches!(&result, Ok(Ok(_))) {
                changed();
            }
            let Some(panel) = weak.upgrade().filter(|p| !p.closed.get()) else {
                return;
            };
            panel.busy.set(false);
            panel.widget.set_sensitive(true);
            panel.invalidate();
            match result {
                Ok(Ok((result, current))) => match result {
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
                        if let Ok(path) = current {
                            panel
                                .current
                                .set_text(&format!("Current location: {}", path.display()));
                        }
                        panel.load();
                    }
                    Err(error) => {
                        panel.status.set_text(&error);
                        panel.load();
                    }
                },
                Ok(Err(error)) => {
                    panel.status.set_text(&error);
                    panel.load();
                }
                Err(_) => {
                    panel.status.set_text(
                        "The location update could not finish. Check the current setting before retrying.",
                    );
                    panel.load();
                }
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
    fn location_panel_applies_once_and_rejects_invalid_busy_stale_and_closed_changes() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let home = root.path().join("home");
        let config = root.path().join("config");
        let target = home.join("Incoming");
        let elsewhere = home.join("Elsewhere");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir_all(home.join("Downloads")).unwrap();
        fs::create_dir(&config).unwrap();
        let original = "XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n";
        fs::write(config.join("user-dirs.dirs"), original).unwrap();
        fs::write(home.join("Downloads/keep.txt"), "keep").unwrap();
        let locations = FolderLocations::new(home.clone(), &config);
        let idle = Rc::new(Cell::new(true));
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
        let apply = gtk::Button::with_label("Apply");
        panel.attach_apply(&apply);
        wait_until("initial location", || {
            !panel.loading.get() && !panel.entry.text().is_empty()
        });
        assert!(!apply.is_sensitive(), "the current location is already selected");

        panel.entry.set_text("/tmp");
        apply.emit_clicked();
        wait_until("invalid apply", || !panel.busy.get() && !panel.loading.get());
        assert!(panel.status.text().contains("persistent location"));
        idle.set(false);
        assert_eq!(fs::read_to_string(locations.user_dirs_file()).unwrap(), original);

        panel.entry.set_text(target.to_str().unwrap());
        apply.emit_clicked();
        assert!(panel.status.text().contains("file operations"));
        assert_eq!(fs::read_to_string(locations.user_dirs_file()).unwrap(), original);
        idle.set(true);
        let guard = FolderChangeGuard::acquire().unwrap();
        apply.emit_clicked();
        assert!(panel
            .status
            .text()
            .contains("Another folder location change is running."));
        drop(guard);

        assert!(apply.is_sensitive());
        let before_apply_refreshes = refreshes.get();
        apply.emit_clicked();
        assert!(!panel.widget.is_sensitive());
        wait_until("applied", || !panel.busy.get() && !panel.loading.get());
        assert!(
            panel.status.text().contains("Location updated"),
            "{}",
            panel.status.text()
        );
        assert_eq!(refreshes.get(), before_apply_refreshes + 1);
        assert_eq!(locations.current_path(KnownFolder::Downloads).unwrap(), target);
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
        assert!(apply.is_sensitive());

        fs::write(
            locations.user_dirs_file(),
            format!("XDG_DOWNLOAD_DIR=\"{}\"\n", elsewhere.display()),
        )
        .unwrap();
        panel.entry.set_text(target.to_str().unwrap());
        apply.emit_clicked();
        wait_until("stale current location", || {
            !panel.busy.get() && !panel.loading.get()
        });
        assert!(panel.status.text().contains("current location changed"));
        assert_eq!(locations.current_path(KnownFolder::Downloads).unwrap(), elsewhere);
        assert_eq!(panel.entry.text().as_str(), elsewhere.to_str().unwrap());

        panel.entry.set_text("/tmp");
        apply.emit_clicked();
        panel.cancel();
        let weak = Rc::downgrade(&panel);
        drop(panel);
        assert!(
            weak.upgrade().is_none(),
            "signals and pending reads do not retain a closed panel"
        );
        wait_until("closed location worker", || !FolderChangeGuard::is_busy());
        assert!(!FolderChangeGuard::is_busy());
    }
}
