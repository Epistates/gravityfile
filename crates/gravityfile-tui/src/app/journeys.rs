//! End-to-end journeys: key presses and clicks on a real scanned directory,
//! asserting on what a person would see and on the filesystem.

use super::harness::Harness;
use super::state::{LayoutMode, SortMode};
use super::{AppMode, UserSettings};

// --- Deleting -----------------------------------------------------------

#[tokio::test]
async fn deleting_a_selected_file_removes_it_from_disk_and_view() {
    let mut harness = Harness::scanned().await;
    harness.select("copy-b.bin").await;
    harness.press("d").await;
    assert_eq!(harness.app.mode, AppMode::ConfirmDelete);
    harness.press("y").await;
    assert!(!harness.path("copy-b.bin").exists());
    assert!(!harness.tree_shows("copy-b.bin"), "{}", harness.screen());
    assert!(harness.screen().contains("copy-a.bin"));
}

#[tokio::test]
async fn rebound_keys_take_effect_and_the_delete_dialog_keeps_its_own() {
    let mut harness = Harness::scanned().await;
    let file: crate::keymap::KeymapFile = toml::from_str(
        r#"
        [[keymap]]
        on = "<C-n>"
        run = "move_down"

        [[keymap]]
        on = "y"
        run = "none"

        [[keymap]]
        on = "X"
        run = "delete"
        "#,
    )
    .unwrap();
    assert!(harness.app.keymap.apply(&file).is_empty());
    harness.select("copy-a.bin").await;
    harness.press("<c-n>").await;
    assert_eq!(harness.selected(), Some(harness.path("copy-b.bin")));
    // With y unbound, copying has no key, so the footer stops offering it.
    let screen = harness.screen();
    assert!(
        !screen.contains("Copy") && screen.contains("Cut"),
        "{screen}"
    );
    // y is unbound in the explorer but still confirms a delete.
    harness.press("Xy").await;
    assert!(!harness.path("copy-b.bin").exists());
}

#[tokio::test]
async fn the_preview_scrolls_with_keys_and_the_wheel() {
    let mut harness = Harness::scanned_sized(120, 30).await;
    let text: String = (1..=200).map(|n| format!("line {n:03}\n")).collect();
    std::fs::write(harness.path("long.txt"), text).unwrap();
    harness.press("R").await;
    harness.press("v").await;
    harness.select("long.txt").await;
    assert!(
        harness.screen().contains("line 001"),
        "{}",
        harness.screen()
    );

    harness.press("JJ").await;
    let screen = harness.screen();
    assert!(
        !screen.contains("line 010") && screen.contains("line 011"),
        "{screen}"
    );
    harness.press("K").await;
    assert!(
        harness.screen().contains("line 006"),
        "{}",
        harness.screen()
    );

    // The wheel over the preview scrolls it rather than the selection.
    harness.scroll_down(110, 15).await;
    assert!(
        harness.screen().contains("line 011"),
        "{}",
        harness.screen()
    );
    assert_eq!(harness.selected(), Some(harness.path("long.txt")));

    // Scrolling stops at the end, and a new selection starts at the top.
    harness
        .press("JJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJJ")
        .await;
    assert!(
        harness.screen().contains("line 200"),
        "{}",
        harness.screen()
    );
    harness.press("kj").await;
    assert!(
        harness.screen().contains("line 001"),
        "{}",
        harness.screen()
    );
}

#[tokio::test]
async fn a_snapshot_is_browsed_read_only_without_touching_the_filesystem() {
    let mut harness = Harness::snapshot().await;
    let screen = harness.screen();
    assert!(screen.contains("snapshot home.json, read-only"), "{screen}");
    assert!(harness.tree_shows("copy-a.bin"), "{screen}");

    // Browsing works from the record, though none of these paths exist.
    harness.select("src").await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src"));
    assert!(harness.tree_shows("modified.rs"), "{}", harness.screen());
    harness.press("<bs><bs>").await;
    assert_eq!(harness.app.nav.view_root, harness.root(), "stays inside");
    assert!(harness.app.error.is_none(), "{:?}", harness.app.error);
    harness.press("b").await;
    assert!(harness.screen().contains(" apparent in "));

    // Nothing that would change files or rescan runs.
    let refused = |harness: &Harness| {
        harness
            .app
            .operation_message
            .as_ref()
            .is_some_and(|(ok, message)| !ok && message.contains("snapshot"))
    };
    harness.select("copy-a.bin").await;
    for keys in ["d", "r", "R", "a", "<c-z>"] {
        harness.app.operation_message = None;
        harness.press(keys).await;
        assert_eq!(harness.app.mode, AppMode::Normal, "{keys}");
        assert!(refused(&harness), "{keys}");
    }
    harness.app.operation_message = None;
    harness.press(":").await;
    harness.type_text("mkdir new").await;
    harness.press("<enter>").await;
    assert!(refused(&harness));

    // Duplicates need file contents; the view says so instead of searching.
    harness.press("<tab>").await;
    assert_eq!(harness.app.view, super::View::Duplicates);
    assert!(
        harness.screen().contains("compares file contents"),
        "{}",
        harness.screen()
    );
}

#[tokio::test]
async fn the_whole_help_can_be_scrolled_into_view() {
    let mut harness = Harness::scanned_sized(70, 24).await;
    harness.press("?").await;
    assert!(
        harness.screen().contains("Move down"),
        "{}",
        harness.screen()
    );
    let (last_command, _) = *crate::event::get_command_help().last().unwrap();
    assert!(!harness.screen().contains(last_command));
    harness.press("G").await;
    assert!(
        harness.screen().contains(last_command),
        "{}",
        harness.screen()
    );
    harness.press("<esc>").await;
    assert_eq!(harness.app.mode, AppMode::Normal);
}

#[tokio::test]
async fn delete_never_targets_the_directory_being_viewed() {
    let mut harness = Harness::scanned().await;
    harness.press("g").await;
    assert_eq!(harness.selected().as_deref(), Some(harness.root()));
    harness.press("dy").await;
    assert!(harness.root().exists());
    assert!(harness.path("copy-a.bin").exists());
}

#[tokio::test]
async fn declining_a_delete_keeps_the_marks_you_made() {
    let mut harness = Harness::scanned().await;
    harness.select("copy-a.bin").await;
    harness.press("<space>").await;
    harness.select("empty").await;
    harness.press("dn").await;
    assert!(harness.app.marked.contains(&harness.path("copy-a.bin")));
    assert!(!harness.app.marked.contains(&harness.path("empty")));
    assert!(harness.path("copy-a.bin").exists());
}

#[tokio::test]
async fn deleting_inside_a_subdirectory_keeps_the_whole_scan() {
    let mut harness = Harness::scanned().await;
    let root = harness.root().to_path_buf();
    harness.select("src").await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src"));
    harness.select("modified.rs").await;
    harness.press("dy").await;
    assert!(!harness.path("src/modified.rs").exists());

    // The scan still covers the root it started from, with real sizes.
    assert_eq!(harness.app.path, root);
    let tree = harness.app.tree.as_ref().expect("a tree");
    assert_eq!(tree.root_path, root);
    harness.press("<bs>").await;
    assert_eq!(harness.app.nav.view_root, root);
    let screen = harness.screen();
    let copy = screen
        .lines()
        .find(|line| line.contains("copy-a.bin"))
        .unwrap();
    assert!(
        copy.contains("kB"),
        "sizes are known after going back:\n{screen}"
    );
}

// --- Searching and sorting ----------------------------------------------

#[tokio::test]
async fn search_uses_smart_case() {
    let mut harness = Harness::scanned().await;
    std::fs::write(harness.path("README.md"), "read me").unwrap();
    harness.press("R").await;
    let mut search = async |query: &str| {
        harness.press("/").await;
        assert_eq!(harness.app.mode, AppMode::Search);
        harness.type_text(query).await;
        let names: Vec<String> = (harness.app.search_state.results.iter())
            .map(|result| result.display.clone())
            .collect();
        harness.press("<esc>").await;
        names
    };
    // Lowercase ignores case; a capital makes the query exact.
    assert!(
        search("readme")
            .await
            .iter()
            .any(|name| name == "README.md")
    );
    assert!(
        search("README")
            .await
            .iter()
            .any(|name| name == "README.md")
    );
    assert!(search("Readme").await.is_empty());
    assert!(search("COPY").await.is_empty());
}

#[tokio::test]
async fn search_facets_narrow_by_size_type_and_extension() {
    let mut harness = Harness::scanned().await;
    let mut search = async |query: &str| {
        harness.press("/").await;
        harness.type_text(query).await;
        let names: Vec<String> = (harness.app.search_state.results.iter())
            .map(|result| result.display.clone())
            .collect();
        harness.press("<esc>").await;
        names
    };
    // Facets alone list matches largest first; directories are excluded by type.
    let large = search("size>=8kb type:f").await;
    assert!(
        large.len() == 2 && large.iter().all(|name| name.starts_with("copy-")),
        "{large:?}"
    );
    // Facets combine with search text.
    assert_eq!(search("copy ext:bin size>1mb").await, Vec::<String>::new());
    assert_eq!(search("ext:rs").await, ["src/modified.rs"]);
    // A malformed facet is just text, which here matches nothing.
    assert!(search("size>lots").await.is_empty());
}

#[tokio::test]
async fn sort_order_survives_a_rescan() {
    let mut harness = Harness::scanned().await;
    while harness.app.sort_mode != SortMode::NameAscending {
        harness.press("s").await;
    }
    let order = |screen: &str| {
        ["a-very-long", "copy-a.bin", "empty", "src"].map(|name| {
            screen
                .find(name)
                .unwrap_or_else(|| panic!("{name}:\n{screen}"))
        })
    };
    let before = order(&harness.screen());
    assert!(before.is_sorted(), "{}", harness.screen());
    harness.press("R").await;
    assert!(order(&harness.screen()).is_sorted(), "{}", harness.screen());
}

// --- Navigating ---------------------------------------------------------

#[tokio::test]
async fn going_back_selects_the_directory_left_after_a_resort_and_layout_switch() {
    let mut harness = Harness::scanned().await;
    harness.select("src").await;
    harness.press("<enter>").await;
    // Inside src, re-sort by name and switch to Miller columns.
    while harness.app.sort_mode != SortMode::NameAscending {
        harness.press("s").await;
    }
    harness.press("v").await;
    assert_eq!(harness.app.layout_mode, LayoutMode::Miller);
    harness.press("<bs>").await;
    assert_eq!(harness.app.nav.view_root, harness.root());
    assert_eq!(harness.selected(), Some(harness.path("src")));
    // Entering again and coming back the other way round works too.
    harness.press("<enter>v<bs>").await;
    assert_eq!(harness.app.layout_mode, LayoutMode::Tree);
    assert_eq!(harness.selected(), Some(harness.path("src")));
}

#[tokio::test]
async fn backspace_goes_back_in_the_treemap() {
    let mut harness = Harness::scanned().await;
    harness.select("src").await;
    harness.press("<enter>").await;
    while harness.app.view != super::View::Treemap {
        harness.press("<tab>").await;
    }
    harness.press("<bs>").await;
    assert_eq!(harness.app.nav.view_root, harness.root());
}

#[tokio::test]
async fn going_to_an_ancestor_of_the_scan_root_lists_it() {
    // The fixture is a hidden `.tmp…` directory.
    let settings = UserSettings {
        show_hidden: true,
        scan_on_startup: false,
        ..UserSettings::default()
    };
    let mut harness = Harness::scanned_with(100, 30, settings).await;
    let parent = harness.root().parent().unwrap().to_path_buf();
    let name = harness
        .root()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // The prompt starts with the current path; Ctrl-U clears it.
    harness.press("<c-g><c-u>").await;
    harness.type_text(&parent.to_string_lossy()).await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.nav.view_root, parent);
    // The parent is shared with other tests, so the row may be off-screen.
    let tree = harness.app.tree.as_ref().expect("a listing");
    assert_eq!(tree.root_path, parent);
    assert!(tree.root.children.iter().any(|child| child.name == name));
    // Going back returns to the scanned tree's directory.
    harness.press("<bs>").await;
    assert_eq!(harness.app.nav.view_root, harness.root());
}

#[tokio::test]
async fn each_tab_keeps_its_own_place_selection_and_history() {
    let mut harness = Harness::scanned().await;
    let root = harness.root().to_path_buf();
    // Tab 1: into src, selecting modified.rs.
    harness.select("src").await;
    harness.press("<enter>").await;
    harness.select("modified.rs").await;
    // Tab 2 opens on src too, then goes into nested.
    harness.press("<c-t>").await;
    harness.select("nested").await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src/nested"));

    // Back on tab 1, the selection survived and Backspace follows tab 1's
    // own history, not tab 2's.
    harness.press("1").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src"));
    assert_eq!(harness.selected(), Some(harness.path("src/modified.rs")));
    harness.press("<bs>").await;
    assert_eq!(harness.app.nav.view_root, root);

    // Tab 2 is still where it was left.
    harness.press("2").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src/nested"));
    harness.press("<bs>").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src"));
}

#[tokio::test]
async fn a_tab_whose_directory_was_deleted_elsewhere_moves_to_what_remains() {
    let mut harness = Harness::scanned().await;
    let root = harness.root().to_path_buf();
    harness.press("<c-t>").await;
    harness.select("src").await;
    harness.press("<enter>").await;
    harness.select("nested").await;
    harness.press("<enter>").await;
    // Tab 1 deletes src, and tab 2's directory with it.
    harness.press("1").await;
    harness.select("src").await;
    harness.press("dy").await;
    assert!(!harness.path("src").exists());
    harness.press("2").await;
    assert_eq!(harness.app.nav.view_root, root);
    assert!(harness.tree_shows("copy-a.bin"), "{}", harness.screen());
}

#[tokio::test]
async fn closing_a_tab_returns_to_the_other_tabs_directory() {
    let mut harness = Harness::scanned().await;
    let root = harness.root().to_path_buf();
    harness.press("<c-t>").await;
    assert_eq!(harness.app.tab_manager.len(), 2);
    harness.select("src").await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src"));
    harness.press("<c-w>").await;
    assert_eq!(harness.app.tab_manager.len(), 1);
    assert_eq!(harness.app.nav.view_root, root);
    assert!(
        harness.screen().contains("copy-a.bin"),
        "{}",
        harness.screen()
    );
}

#[tokio::test]
async fn the_saved_default_layout_is_used_at_startup() {
    let settings = UserSettings {
        default_layout: LayoutMode::Miller,
        ..UserSettings::default()
    };
    let harness = Harness::scanned_with(100, 30, settings).await;
    assert_eq!(harness.app.layout_mode, LayoutMode::Miller);
}

// --- Drawing and the mouse ----------------------------------------------

#[tokio::test]
async fn sizes_line_up_at_every_depth() {
    let harness = Harness::scanned().await;
    let screen = harness.screen();
    let column = |name: &str| {
        let line = screen.lines().find(|line| line.contains(name)).unwrap();
        line.chars().position(|c| c == 'k').expect("a size in kB")
    };
    assert_eq!(column("copy-a.bin"), column(" src"), "\n{screen}");
}

#[tokio::test]
async fn double_clicking_a_directory_enters_it() {
    let mut harness = Harness::scanned().await;
    let row = harness.row_of(" src");
    harness.click(10, row).await;
    harness.click(10, row).await;
    assert_eq!(harness.app.nav.view_root, harness.path("src"));
}

#[tokio::test]
async fn clicks_after_scrolling_select_the_row_under_the_pointer() {
    // Five rows of entries fit; the sixth needs scrolling.
    let mut harness = Harness::scanned_sized(100, 10).await;
    harness.select("empty").await;
    let row = harness.row_of("copy-b.bin");
    harness.click(10, row).await;
    assert_eq!(harness.selected(), Some(harness.path("copy-b.bin")));
}

#[tokio::test]
async fn right_clicking_marks_the_row_under_the_pointer_after_scrolling() {
    let mut harness = Harness::scanned_sized(100, 10).await;
    let row = harness.row_of("copy-a.bin");
    // Scrolling moves the selection three rows and the view with it.
    harness.scroll_down(10, row).await;
    harness.scroll_down(10, row).await;
    let row = harness.row_of("copy-b.bin");
    harness.right_click(10, row).await;
    assert!(harness.app.marked.contains(&harness.path("copy-b.bin")));
    assert_eq!(harness.app.marked.len(), 1);
}

// --- Changing files keeps the rest of the scan --------------------------

/// The size shown for `name` in the tree, as opposed to the details pane.
fn size_on_screen(harness: &Harness, name: &str) -> String {
    let screen = harness.screen();
    let after = screen
        .lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(name)?;
            (before.contains("├─") || before.contains("└─")).then_some(after)
        })
        .next()
        .unwrap_or_else(|| panic!("{name} is not a tree row:\n{screen}"));
    after
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::test]
async fn b_switches_between_disk_usage_and_apparent_sizes_without_rescanning() {
    let mut harness = Harness::scanned().await;
    std::fs::write(harness.path("small.txt"), b"0123456789").unwrap();
    harness.press("R").await;
    harness.select("small.txt").await;
    let on_disk = size_on_screen(&harness, "small.txt");
    assert_ne!(on_disk, "10 B", "a 10-byte file occupies a whole block");

    harness.press("b").await;
    assert_eq!(size_on_screen(&harness, "small.txt"), "10 B");
    assert!(
        harness.screen().contains(" apparent in "),
        "{}",
        harness.screen()
    );
    assert_eq!(harness.selected(), Some(harness.path("small.txt")));
    // A rescan keeps the chosen measure.
    harness.press("R").await;
    assert_eq!(size_on_screen(&harness, "small.txt"), "10 B");

    harness.press("b").await;
    assert_eq!(size_on_screen(&harness, "small.txt"), on_disk);
}

#[tokio::test]
async fn pasting_into_a_subdirectory_updates_sizes_in_place() {
    let mut harness = Harness::scanned().await;
    let root = harness.root().to_path_buf();
    let before = harness.app.tree.as_ref().unwrap().root.size;
    harness.select("copy-a.bin").await;
    harness.press("y").await;
    harness.select("src").await;
    harness.press("<enter>p").await;
    assert!(harness.path("src/copy-a.bin").is_file());
    assert_eq!(harness.app.path, root, "the scan root is unchanged");
    assert!(
        harness.screen().contains("copy-a.bin"),
        "{}",
        harness.screen()
    );
    let tree = harness.app.tree.as_ref().unwrap();
    let copied = tree.find(&harness.path("src/copy-a.bin")).unwrap().size;
    assert!(copied > 0);
    assert_eq!(tree.root.size, before + copied);
    assert_eq!(
        tree.find(&harness.path("src")).unwrap().file_count(),
        3,
        "nested/deep.txt, modified.rs and the copy"
    );
    // Nothing else was rescanned away: the sibling still has its size.
    harness.press("<bs>").await;
    assert_eq!(size_on_screen(&harness, "copy-b.bin"), "8.19 kB");
}

#[tokio::test]
async fn renaming_updates_the_entry_in_place() {
    let mut harness = Harness::scanned().await;
    harness.select("copy-b.bin").await;
    harness.press("r<c-u>").await;
    harness.type_text("renamed.bin").await;
    harness.press("<enter>").await;
    assert!(harness.path("renamed.bin").is_file());
    let screen = harness.screen();
    assert!(
        screen.contains("renamed.bin") && !screen.contains("copy-b.bin"),
        "{screen}"
    );
    assert_eq!(size_on_screen(&harness, "renamed.bin"), "8.19 kB");
}

#[tokio::test]
async fn creating_a_file_adds_it_in_place() {
    let mut harness = Harness::scanned().await;
    let files = harness.app.tree.as_ref().unwrap().root.file_count();
    harness.press("a").await;
    harness.type_text("notes.txt").await;
    harness.press("<enter>").await;
    assert!(harness.path("notes.txt").is_file());
    assert!(
        harness.screen().contains("notes.txt"),
        "{}",
        harness.screen()
    );
    assert_eq!(
        harness.app.tree.as_ref().unwrap().root.file_count(),
        files + 1
    );
}

#[tokio::test]
async fn an_invalid_name_keeps_the_prompt_open_with_the_reason() {
    let mut harness = Harness::scanned().await;
    harness.press("a").await;
    harness.type_text("a/b").await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.mode, AppMode::CreatingFile);
    assert!(
        harness.screen().contains("Name cannot contain '/'"),
        "{}",
        harness.screen()
    );
    // Fixing the name in the same prompt creates the file.
    harness.press("<bs><bs><bs>").await;
    harness.type_text("back\\slash").await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.mode, AppMode::Normal);
    #[cfg(unix)]
    assert!(harness.path("back\\slash").is_file());
}

#[tokio::test]
async fn unread_directories_are_listed_in_the_background() {
    let mut harness = Harness::scanned().await;
    // A quick listing, as before a scan finishes: subdirectories are unread.
    let quick = gravityfile_scan::quick_list(harness.root(), None).unwrap();
    harness.app.install_tree(quick);
    harness.app.has_full_scan = false;
    harness.app.view_changed();
    harness.press("v").await;
    harness.select("src").await;
    harness.press_unsettled("<enter>").await;
    assert_eq!(harness.app.nav.view_root, harness.path("src"));
    assert!(
        !harness.app.listings.is_empty(),
        "the listing runs off the UI thread"
    );
    assert!(
        harness.screen().contains("(loading…)"),
        "{}",
        harness.screen()
    );
    harness.settle().await;
    assert!(harness.app.listings.is_empty());
    assert!(
        harness.screen().contains("modified.rs"),
        "{}",
        harness.screen()
    );
}

#[tokio::test]
async fn rescanning_a_subdirectory_keeps_the_rest_of_the_scan() {
    let mut harness = Harness::scanned().await;
    let root = harness.root().to_path_buf();
    harness.select("src").await;
    harness.press("<enter>").await;
    std::fs::write(harness.path("src/added.bin"), vec![0u8; 50_000]).unwrap();
    harness.press("R").await;
    assert_eq!(harness.app.path, root);
    assert!(
        harness.screen().contains("added.bin"),
        "{}",
        harness.screen()
    );
    harness.press("<bs>").await;
    assert_eq!(size_on_screen(&harness, "copy-a.bin"), "8.19 kB");
}

#[tokio::test]
async fn an_operation_during_a_scan_leaves_the_scan_running() {
    let mut harness = Harness::scanned().await;
    harness.select("copy-b.bin").await;
    harness.press_unsettled("R").await;
    assert!(harness.app.scan_rx.is_some(), "a full scan is running");
    let scan = harness.app.scan_token.clone();
    harness.press_unsettled("dy").await;
    assert!(harness.app.op_rx.is_some(), "the delete is running");
    assert!(
        !scan.is_cancelled(),
        "starting the delete must not cancel the scan"
    );
    harness.settle().await;
    assert!(!harness.path("copy-b.bin").exists());
    assert!(!harness.tree_shows("copy-b.bin"), "{}", harness.screen());
    assert!(harness.screen().contains("copy-a.bin"));
}
