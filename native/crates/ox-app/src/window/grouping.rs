// SPDX-License-Identifier: AGPL-3.0-only
//! Per-tab date grouping and the live standard Downloads folder default.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use ox_core::places::{KnownFolder, Place};

use crate::folder_view::sorting::{DateGrouping, GroupingMode};

use super::window_action::WindowAction;
use super::BrowserWindow;

/// Automatic grouping applies to the configured Downloads root only.
fn is_grouped(mode: GroupingMode, uri: &str, folders: &[Place]) -> bool {
    match mode {
        GroupingMode::Automatic => folders.iter().any(|place| {
            place.known_folder == Some(KnownFolder::Downloads)
                && gtk::gio::File::for_uri(&place.uri).equal(&gtk::gio::File::for_uri(uri))
        }),
        GroupingMode::DateModified => true,
        GroupingMode::None => false,
    }
}

impl BrowserWindow {
    /// Refreshes relative dates when an open window crosses local midnight.
    pub(super) fn follow_grouping_calendar(&self) {
        let window = self.downgrade();
        gtk::glib::timeout_add_local(std::time::Duration::from_mins(1), move || {
            let Some(window) = window.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            window.update_date_grouping();
            gtk::glib::ControlFlow::Continue
        });
    }

    /// Updates grouping after navigation, a tab switch, or a standard-folder change.
    pub(super) fn update_date_grouping(&self) {
        let Some((mode, uri)) = self
            .imp()
            .session
            .borrow()
            .active()
            .map(|tab| (tab.grouping, tab.uri().to_owned()))
        else {
            return;
        };
        self.set_action_state(WindowAction::Grouping, &mode.as_str().to_variant());
        let grouping = is_grouped(mode, &uri, &self.context().known_folders())
            .then(DateGrouping::current)
            .flatten();
        let model = self.folder_pane().model();
        if model.date_grouping() == grouping {
            return;
        }
        let selected = model.selected_uris();
        self.change_model(|| {
            self.folder_pane().set_date_grouping(grouping);
            model.select_uris(&selected);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::harness::{capture, descendants, wait_until, Fixture, TestWindow};

    /// parity: VIEW-022
    #[gtk::test]
    fn date_grouping_shows_headings_and_keeps_selection_sorting_and_tab_choice() {
        let fixture = Fixture::standard();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_hours(262_968);
        std::fs::File::open(fixture.path("Notes 2.txt"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let test = TestWindow::open(&fixture.uri());
        let model = test.window.folder_pane().model();
        assert!(model.date_grouping().is_none());
        model.select_only(test.position_of("Notes 2.txt"));
        let selected = model.selected_uris();
        test.activate("grouping", Some("date-modified"));
        test.activate("sort", Some("name"));
        test.activate("direction", Some("descending"));
        assert_eq!(model.selected_uris(), selected);
        let heading_labels = || {
            descendants::<gtk::Label>(test.window.folder_pane().details().column_view())
                .into_iter()
                .filter(|label| label.has_css_class("group-heading"))
                .map(|label| label.text().to_string())
                .collect::<Vec<_>>()
        };
        wait_until("date section headings", || {
            let labels = heading_labels();
            labels.contains(&"Today".to_owned()) && labels.contains(&"Long time ago".to_owned())
        });
        assert_eq!(model.n_items(), 4, "headings never become file rows");
        assert_eq!(model.sorted().section(0), (0, 3));
        assert_eq!(model.sorted().section(3), (3, 4));
        capture(&test.window, "native-date-groups.png");
        test.activate("view", Some("icons"));
        assert_eq!(model.selected_uris(), selected);
        test.activate("view", Some("details"));
        let first = test.window.imp().session.borrow().active_id().unwrap();
        test.window.add_tab(&fixture.uri_of("Documents")).unwrap();
        test.wait_for_listing("ungrouped second tab");
        assert!(model.date_grouping().is_none());
        assert_eq!(test.action_state("grouping").as_deref(), Some("automatic"));
        test.window.imp().session.borrow_mut().activate(first);
        test.window.show_tab(first);
        assert!(model.date_grouping().is_some());
        assert_eq!(test.action_state("grouping").as_deref(), Some("date-modified"));
        assert_eq!(model.selected_uris(), selected);
        test.window.navigate(&fixture.uri_of("Documents")).unwrap();
        test.wait_for_listing("explicit grouping follows the tab");
        assert!(model.date_grouping().is_some());
        test.activate("grouping", Some("none"));
        assert!(model.date_grouping().is_none());
        assert!(test
            .window
            .folder_pane()
            .details()
            .column_view()
            .header_factory()
            .is_none());
    }

    #[test]
    fn automatic_grouping_follows_only_the_configured_downloads_root() {
        let folders = [Place {
            label: "Downloads".to_owned(),
            uri: "file:///home/example/Incoming".to_owned(),
            known_folder: Some(KnownFolder::Downloads),
            is_shared: false,
        }];
        assert!(is_grouped(GroupingMode::Automatic, &folders[0].uri, &folders));
        for uri in [
            "file:///home/example/Downloads",
            "file:///home/example/Incoming/Child",
            "file:///tmp/Downloads",
            "file:///home/example/Documents",
        ] {
            assert!(!is_grouped(GroupingMode::Automatic, uri, &folders));
            assert!(is_grouped(GroupingMode::DateModified, uri, &folders));
        }
        assert!(!is_grouped(GroupingMode::None, &folders[0].uri, &folders));
        assert!(!is_grouped(GroupingMode::Automatic, &folders[0].uri, &[]));
    }
}
