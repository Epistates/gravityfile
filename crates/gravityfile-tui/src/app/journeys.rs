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
    assert!(
        !harness.screen().contains("copy-b.bin"),
        "{}",
        harness.screen()
    );
    assert!(harness.screen().contains("copy-a.bin"));
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
    assert_eq!(harness.app.view_root, harness.path("src"));
    harness.select("modified.rs").await;
    harness.press("dy").await;
    assert!(!harness.path("src/modified.rs").exists());

    // The scan still covers the root it started from, with real sizes.
    assert_eq!(harness.app.path, root);
    let tree = harness.app.tree.as_ref().expect("a tree");
    assert_eq!(tree.root_path, root);
    harness.press("<bs>").await;
    assert_eq!(harness.app.view_root, root);
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
    assert_eq!(harness.app.view_root, parent);
    // The parent is shared with other tests, so the row may be off-screen.
    let tree = harness.app.tree.as_ref().expect("a listing");
    assert_eq!(tree.root_path, parent);
    assert!(tree.root.children.iter().any(|child| child.name == name));
    // Going back returns to the scanned tree's directory.
    harness.press("<bs>").await;
    assert_eq!(harness.app.view_root, harness.root());
}

#[tokio::test]
async fn closing_a_tab_returns_to_the_other_tabs_directory() {
    let mut harness = Harness::scanned().await;
    let root = harness.root().to_path_buf();
    harness.press("<c-t>").await;
    assert_eq!(harness.app.tab_manager.len(), 2);
    harness.select("src").await;
    harness.press("<enter>").await;
    assert_eq!(harness.app.view_root, harness.path("src"));
    harness.press("<c-w>").await;
    assert_eq!(harness.app.tab_manager.len(), 1);
    assert_eq!(harness.app.view_root, root);
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
    assert_eq!(harness.app.view_root, harness.path("src"));
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
