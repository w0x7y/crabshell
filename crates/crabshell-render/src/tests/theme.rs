//! Finding icons by name in fake icon themes.

use std::path::PathBuf;

use crabshell_ui::{Icon, Style};

use super::{Scene, capture_logs, fixture};
use crate::Renderer;
use crate::theme::{Themes, bases};

/// A fake set of base directories in a temporary directory, removed when
/// the test ends: `home/.icons`, `data/icons` and `pixmaps`, in that order.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(test: &str) -> Tree {
        let root =
            std::env::temp_dir().join(format!("crabshell-render-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Tree { root }
    }

    /// Writes `contents` to `path` under the root.
    fn file(&self, path: &str, contents: &str) -> &Tree {
        let path = self.root.join(path);
        let parent = path.parent().expect("a file has a parent directory");
        std::fs::create_dir_all(parent).expect("create a test directory");
        std::fs::write(&path, contents).expect("write a test file");
        self
    }

    fn bases(&self) -> Vec<PathBuf> {
        ["home/.icons", "data/icons", "pixmaps"]
            .iter()
            .map(|base| self.root.join(base))
            .collect()
    }

    fn themes(&self) -> Themes {
        Themes::new(self.bases())
    }

    /// What `themes` found, relative to the root.
    fn found(
        &self,
        themes: &mut Themes,
        theme: &str,
        name: &str,
        size: u32,
        scale: u32,
    ) -> Option<String> {
        themes.find(theme, name, size, scale).map(|path| {
            path.strip_prefix(&self.root)
                .expect("found under the root")
                .display()
                .to_string()
        })
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn base_directories_follow_the_specs_order() {
    let home = Some(PathBuf::from("/home/u"));
    assert_eq!(
        bases(home.clone(), None, None),
        [
            "/home/u/.icons",
            "/home/u/.local/share/icons",
            "/usr/local/share/icons",
            "/usr/share/icons",
            "/usr/share/pixmaps",
        ]
        .map(PathBuf::from)
    );
    // Relative paths are ignored.
    assert_eq!(
        bases(
            home,
            Some("relative".into()),
            Some("/opt/data:also/relative".into())
        ),
        [
            "/home/u/.icons",
            "/home/u/.local/share/icons",
            "/opt/data/icons",
            "/usr/share/pixmaps",
        ]
        .map(PathBuf::from)
    );
    assert_eq!(
        bases(None, Some("/data".into()), Some("/x".into())),
        ["/data/icons", "/x/icons", "/usr/share/pixmaps"].map(PathBuf::from)
    );
}

#[test]
fn an_exact_size_wins_and_files_can_be_in_another_base() {
    let tree = Tree::new("exact");
    tree.file(
        "home/.icons/fake/index.theme",
        "[Icon Theme]\nDirectories=16,24\n\n[16]\nSize=16\nType=Fixed\n\n[24]\nSize=24\nType=Fixed\n",
    )
    .file("data/icons/fake/16/app.png", "")
    .file("data/icons/fake/24/app.png", "");
    let mut themes = tree.themes();
    let found = tree.found(&mut themes, "fake", "app", 24, 1);
    assert_eq!(found.as_deref(), Some("data/icons/fake/24/app.png"));
}

#[test]
fn the_closest_directory_wins_across_kinds() {
    let tree = Tree::new("closest");
    tree.file(
        "data/icons/fake/index.theme",
        "[Icon Theme]\nDirectories=fixed,threshold,scalable\n\
         [fixed]\nSize=16\nType=Fixed\n\
         [threshold]\nSize=32\nType=Threshold\nThreshold=2\n\
         [scalable]\nSize=48\nType=Scalable\nMinSize=44\nMaxSize=52\n",
    );
    for directory in ["fixed", "threshold", "scalable"] {
        tree.file(&format!("data/icons/fake/{directory}/app.png"), "");
    }
    let mut themes = tree.themes();
    let mut pick = |size| tree.found(&mut themes, "fake", "app", size, 1);
    // Exact: inside the threshold or the scalable range.
    assert_eq!(
        pick(31).as_deref(),
        Some("data/icons/fake/threshold/app.png")
    );
    assert_eq!(
        pick(50).as_deref(),
        Some("data/icons/fake/scalable/app.png")
    );
    // Closest: 37 is 5 past the threshold directory's 32 and 7 short of 44.
    assert_eq!(
        pick(37).as_deref(),
        Some("data/icons/fake/threshold/app.png")
    );
    // 40 is 4 short of the scalable range.
    assert_eq!(
        pick(40).as_deref(),
        Some("data/icons/fake/scalable/app.png")
    );
    // 20 is 4 from the fixed 16 and 12 from the threshold directory's 32.
    assert_eq!(pick(20).as_deref(), Some("data/icons/fake/fixed/app.png"));
}

#[test]
fn scale_2_directories_serve_scale_2() {
    let tree = Tree::new("scale");
    tree.file(
        "data/icons/fake/index.theme",
        "[Icon Theme]\nDirectories=16\nScaledDirectories=16@2\n\
         [16]\nSize=16\nType=Fixed\n[16@2]\nSize=16\nScale=2\nType=Fixed\n",
    )
    .file("data/icons/fake/16/app.png", "")
    .file("data/icons/fake/16@2/app.png", "");
    let mut themes = tree.themes();
    let mut pick = |size, scale| tree.found(&mut themes, "fake", "app", size, scale);
    assert_eq!(pick(16, 2).as_deref(), Some("data/icons/fake/16@2/app.png"));
    assert_eq!(pick(16, 1).as_deref(), Some("data/icons/fake/16/app.png"));
    // 32 at scale 1 matches no directory exactly; 16@2 is 32 pixels too.
    assert_eq!(pick(32, 1).as_deref(), Some("data/icons/fake/16@2/app.png"));
}

#[test]
fn an_exact_match_beats_an_earlier_directory_as_close() {
    let tree = Tree::new("priority");
    tree.file(
        "data/icons/fake/index.theme",
        "[Icon Theme]\nDirectories=16@2,32\n\
         [16@2]\nSize=16\nScale=2\nType=Fixed\n[32]\nSize=32\nType=Fixed\n",
    )
    .file("data/icons/fake/16@2/app.png", "")
    .file("data/icons/fake/32/app.png", "");
    let mut themes = tree.themes();
    // Both are 32 pixels, but only 32 matches scale 1.
    let found = tree.found(&mut themes, "fake", "app", 32, 1);
    assert_eq!(found.as_deref(), Some("data/icons/fake/32/app.png"));
    let found = tree.found(&mut themes, "fake", "app", 16, 2);
    assert_eq!(found.as_deref(), Some("data/icons/fake/16@2/app.png"));
}

#[test]
fn png_is_preferred_over_svg_as_the_spec_orders_them() {
    let tree = Tree::new("png-first");
    tree.file(
        "data/icons/fake/index.theme",
        "[Icon Theme]\nDirectories=apps\n[apps]\nSize=16\n",
    )
    .file("data/icons/fake/apps/app.png", "")
    .file("data/icons/fake/apps/app.svg", "")
    .file("data/icons/fake/apps/other.svg", "");
    let mut themes = tree.themes();
    let found = tree.found(&mut themes, "fake", "app", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/fake/apps/app.png"));
    let found = tree.found(&mut themes, "fake", "other", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/fake/apps/other.svg"));
}

/// A theme with one 16-pixel directory, inheriting `inherits`.
fn theme(inherits: &str) -> String {
    format!("[Icon Theme]\nInherits={inherits}\nDirectories=apps\n[apps]\nSize=16\n")
}

#[test]
fn inherited_themes_are_searched_depth_first() {
    let tree = Tree::new("inherits");
    tree.file("data/icons/a/index.theme", &theme("b, d"))
        .file("data/icons/b/index.theme", &theme("c"))
        .file("data/icons/c/index.theme", &theme(""))
        .file("data/icons/d/index.theme", &theme(""))
        .file("data/icons/c/apps/both.png", "")
        .file("data/icons/d/apps/both.png", "")
        .file("data/icons/d/apps/only-d.png", "");
    let mut themes = tree.themes();
    // b's parent c comes before a's second parent d.
    let found = tree.found(&mut themes, "a", "both", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/c/apps/both.png"));
    let found = tree.found(&mut themes, "a", "only-d", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/d/apps/only-d.png"));
}

#[test]
fn an_inheritance_loop_ends_and_hicolor_still_answers() {
    let tree = Tree::new("loop");
    tree.file("data/icons/a/index.theme", &theme("b"))
        .file("data/icons/b/index.theme", &theme("a"))
        .file("data/icons/hicolor/index.theme", &theme(""))
        .file("data/icons/hicolor/apps/app.png", "");
    let mut themes = tree.themes();
    let found = tree.found(&mut themes, "a", "app", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/hicolor/apps/app.png"));
    assert_eq!(tree.found(&mut themes, "a", "nowhere", 16, 1), None);
}

#[test]
fn a_theme_without_the_icon_falls_back_to_hicolor() {
    let tree = Tree::new("hicolor");
    tree.file("data/icons/fake/index.theme", &theme(""))
        .file("data/icons/fake/apps/other.png", "")
        .file("data/icons/hicolor/index.theme", &theme(""))
        .file("data/icons/hicolor/apps/app.svg", "");
    let mut themes = tree.themes();
    let found = tree.found(&mut themes, "fake", "app", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/hicolor/apps/app.svg"));
    // A theme that doesn't exist is logged once and falls back too.
    let logs = capture_logs(|| {
        assert!(themes.find("absent", "app", 16, 1).is_some());
        assert!(themes.find("absent", "app", 24, 1).is_some());
    });
    assert_eq!(logs.matches("icon theme not found").count(), 1, "{logs}");
}

#[test]
fn unthemed_files_and_pixmaps_come_last() {
    let tree = Tree::new("pixmaps");
    tree.file("data/icons/hicolor/index.theme", &theme(""))
        .file("data/icons/loose.svg", "")
        .file("pixmaps/legacy.png", "")
        .file("pixmaps/loose.png", "");
    let mut themes = tree.themes();
    let found = tree.found(&mut themes, "hicolor", "legacy", 16, 1);
    assert_eq!(found.as_deref(), Some("pixmaps/legacy.png"));
    // An earlier base directory wins.
    let found = tree.found(&mut themes, "hicolor", "loose", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/loose.svg"));
}

#[test]
fn a_malformed_index_theme_is_used_as_far_as_it_goes() {
    let tree = Tree::new("malformed");
    tree.file(
        "data/icons/fake/index.theme",
        "stray line before any section\n[Icon Theme]\nDirectories=good, nosize, , absent, 17\n\
         no equals sign here\n[good]\nSize=16\nType=Bogus\nScale=x\n[nosize]\nType=Fixed\n\
         [17]\nSize=17\nType=Fixed\n",
    )
    .file("data/icons/fake/good/app.png", "")
    .file("data/icons/fake/17/app.png", "")
    .file("data/icons/garbled/index.theme", "\u{0}\u{1}[[[\n=\n")
    .file("data/icons/hicolor/index.theme", &theme(""))
    .file("data/icons/hicolor/apps/app.png", "");
    let mut themes = tree.themes();
    let logs = capture_logs(|| {
        let found = tree.found(&mut themes, "fake", "app", 16, 1);
        assert_eq!(found.as_deref(), Some("data/icons/fake/good/app.png"));
        // An unknown type is the default, Threshold, so 18 is within 16's
        // threshold of 2, while 17 is only closer.
        let found = tree.found(&mut themes, "fake", "app", 18, 1);
        assert_eq!(found.as_deref(), Some("data/icons/fake/good/app.png"));
        // A theme with nothing usable has no directories; hicolor answers.
        let found = tree.found(&mut themes, "garbled", "app", 16, 1);
        assert_eq!(found.as_deref(), Some("data/icons/hicolor/apps/app.png"));
    });
    assert!(logs.contains("stray line before any section"), "{logs}");
    assert!(logs.contains("no equals sign here"), "{logs}");
    assert!(logs.contains(r#"directory="nosize""#), "{logs}");
    assert!(logs.contains(r#"directory="absent""#), "{logs}");
}

#[test]
fn lookups_and_misses_are_cached() {
    let tree = Tree::new("cache");
    tree.file("data/icons/fake/index.theme", &theme(""))
        .file("data/icons/fake/apps/here.png", "");
    let mut themes = tree.themes();
    assert_eq!(tree.found(&mut themes, "fake", "later", 16, 1), None);
    // The miss is remembered, so a file added afterward isn't seen.
    tree.file("data/icons/fake/apps/later.png", "");
    assert_eq!(tree.found(&mut themes, "fake", "later", 16, 1), None);
    // So is a hit, even after the file goes.
    let here = tree.found(&mut themes, "fake", "here", 16, 1);
    assert!(here.is_some());
    let _ = std::fs::remove_file(tree.root.join("data/icons/fake/apps/here.png"));
    assert_eq!(tree.found(&mut themes, "fake", "here", 16, 1), here);
    // Directories are listed once, so a file added afterward isn't seen at
    // another size either, until the bar restarts.
    assert_eq!(tree.found(&mut themes, "fake", "later", 24, 1), None);
}

#[test]
fn an_xpm_is_skipped_and_logged_once() {
    let tree = Tree::new("xpm");
    tree.file("data/icons/fake/index.theme", &theme(""))
        .file("data/icons/fake/apps/old.xpm", "")
        .file("pixmaps/older.xpm", "");
    let mut themes = tree.themes();
    let logs = capture_logs(|| {
        assert_eq!(themes.find("fake", "old", 16, 1), None);
        assert_eq!(themes.find("fake", "old", 24, 1), None);
        assert_eq!(themes.find("fake", "older", 16, 1), None);
    });
    assert_eq!(logs.matches("only available as XPM").count(), 2, "{logs}");
}

#[test]
fn names_that_would_leave_the_directory_are_refused() {
    let tree = Tree::new("unsafe");
    tree.file("data/icons/fake/index.theme", &theme(""))
        .file("data/escape.png", "")
        .file("data/icons/fake/apps/x.png", "");
    let mut themes = tree.themes();
    for name in ["../escape", "apps/x", "", ".."] {
        assert_eq!(
            tree.found(&mut themes, "fake", name, 16, 1),
            None,
            "{name:?}"
        );
    }
    assert_eq!(tree.found(&mut themes, "../icons/fake", "x", 16, 1), None);
}

#[test]
fn a_named_icon_draws_like_its_file() {
    let tree = Tree::new("draw");
    let svg = std::fs::read_to_string(fixture("shapes.svg")).expect("read the fixture");
    tree.file(
        "data/icons/fake/index.theme",
        "[Icon Theme]\nDirectories=scalable/apps\n[scalable/apps]\nSize=16\nType=Scalable\nMinSize=8\nMaxSize=512\n",
    )
    .file("data/icons/fake/scalable/apps/shapes.svg", &svg);
    let draw = |icon: Icon| {
        let mut scene = Scene::new();
        scene.renderer = Renderer::with_icon_dirs(tree.bases());
        scene.renderer.set_icon_theme("fake");
        let panel = scene
            .panel()
            .style(Style::new().background(crabshell_ui::Color::rgb(0, 0, 0)))
            .child(icon.size(24));
        let window = scene.window(panel, 40.0, 30.0);
        scene.full(&window, 1.5)
    };
    let named = draw(Icon::new("shapes"));
    let file = draw(Icon::new(fixture("shapes.svg")));
    assert!(named.pixels == file.pixels);
    // And something was drawn: the icon's blue fill.
    assert!(
        named
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[2] > 200)
    );
}

#[test]
fn a_fractional_scale_looks_for_the_next_whole_scale() {
    let tree = Tree::new("fractional");
    let square = |color: &str| {
        format!(
            "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'>\
             <rect width='16' height='16' fill='{color}'/></svg>"
        )
    };
    tree.file(
        "data/icons/fake/index.theme",
        "[Icon Theme]\nDirectories=16,16@2\n[16]\nSize=16\nType=Fixed\n\
         [16@2]\nSize=16\nScale=2\nType=Fixed\n",
    )
    .file("data/icons/fake/16/square.svg", &square("#ff0000"))
    .file("data/icons/fake/16@2/square.svg", &square("#0000ff"));
    let mut scene = Scene::new();
    scene.renderer = Renderer::with_icon_dirs(tree.bases());
    scene.renderer.set_icon_theme("fake");
    let window = scene.window(scene.panel().child(Icon::new("square")), 16.0, 16.0);
    // At 1.5 the @2 icon, blue, is drawn; at 1 the plain one, red.
    assert_eq!(scene.full(&window, 1.5).pixel(12, 12), [0, 0, 255, 255]);
    assert_eq!(scene.full(&window, 1.0).pixel(8, 8), [255, 0, 0, 255]);
}

#[test]
fn a_miss_reads_each_directory_once_and_then_nothing() {
    let tree = Tree::new("work");
    let directories: Vec<String> = (0..30).map(|size| format!("{}", size + 8)).collect();
    let mut index = format!("[Icon Theme]\nDirectories={}\n", directories.join(","));
    for directory in &directories {
        index.push_str(&format!("[{directory}]\nSize={directory}\nType=Fixed\n"));
        tree.file(&format!("data/icons/fake/{directory}/present.png"), "");
    }
    tree.file("data/icons/fake/index.theme", &index);
    // The same base twice, and two bases without the theme.
    let mut bases = tree.bases();
    bases.insert(1, bases[1].clone());
    let mut themes = Themes::new(bases);
    assert_eq!(themes.find("fake", "absent", 16, 1), None);
    let (read, checked, _) = themes.work();
    // The 30 directories, each once, and each base directory itself for
    // the unthemed fallback; nothing twice for the duplicated base.
    assert!(read <= 30 + 3, "{read} directories read");
    let mut once = tree.themes();
    once.find("fake", "absent", 16, 1);
    assert_eq!(checked, once.work().1, "the duplicate base adds no checks");
    assert_eq!(themes.find("fake", "also-absent", 16, 1), None);
    assert!(themes.find("fake", "present", 20, 1).is_some());
    assert_eq!(themes.work().0, read, "a second lookup reads no directory");
}

#[test]
fn remembered_lookups_are_capped() {
    let tree = Tree::new("bounded");
    tree.file("data/icons/fake/index.theme", &theme(""))
        .file("data/icons/fake/apps/kept.png", "");
    let mut themes = tree.themes();
    for number in 0..1500 {
        assert_eq!(
            themes.find("fake", &format!("missing-{number}"), 16, 1),
            None
        );
        // A name looked up often stays remembered.
        assert!(themes.find("fake", "kept", 16, 1).is_some());
    }
    assert!(
        themes.work().2 <= 1024,
        "{} lookups remembered",
        themes.work().2
    );
    let _ = std::fs::remove_file(tree.root.join("data/icons/fake/apps/kept.png"));
    assert!(
        themes.find("fake", "kept", 16, 1).is_some(),
        "still remembered"
    );
}

#[test]
fn the_least_recently_used_lookup_is_forgotten_first() {
    let tree = Tree::new("recency");
    tree.file("data/icons/fake/index.theme", &theme(""))
        .file("data/icons/fake/apps/kept.png", "");
    let mut themes = tree.themes();
    assert!(themes.find("fake", "kept", 16, 1).is_some());
    for number in 0..1023 {
        themes.find("fake", &format!("missing-{number}"), 16, 1);
    }
    assert_eq!(themes.work().2, 1024, "full");
    // Used again, `kept` is now the most recent; the next new lookup
    // forgets `missing-0` instead.
    assert!(themes.find("fake", "kept", 16, 1).is_some());
    themes.find("fake", "missing-1023", 16, 1);
    assert_eq!(themes.work().2, 1024);
    let _ = std::fs::remove_file(tree.root.join("data/icons/fake/apps/kept.png"));
    assert!(
        themes.find("fake", "kept", 16, 1).is_some(),
        "still remembered"
    );
}

#[test]
fn a_listed_name_that_isnt_a_file_is_passed_over() {
    let tree = Tree::new("not-a-file");
    tree.file("data/icons/fake/index.theme", &theme(""))
        .file("data/icons/fake/apps/folder.png/inside", "")
        .file("data/icons/fake/apps/folder.svg", "");
    let mut themes = tree.themes();
    assert_eq!(
        tree.found(&mut themes, "fake", "folder", 16, 1).as_deref(),
        Some("data/icons/fake/apps/folder.svg")
    );
}

#[test]
fn an_index_theme_with_invalid_utf8_is_read_anyway() {
    let tree = Tree::new("latin1");
    let path = tree.root.join("data/icons/fake/index.theme");
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create the theme");
    let mut index = b"[Icon Theme]\nName=Caf\xe9\nDirectories=apps\n".to_vec();
    index.extend_from_slice(b"[apps]\nSize=16\n");
    std::fs::write(&path, index).expect("write the index");
    tree.file("data/icons/fake/apps/app.png", "");
    let mut themes = tree.themes();
    let found = tree.found(&mut themes, "fake", "app", 16, 1);
    assert_eq!(found.as_deref(), Some("data/icons/fake/apps/app.png"));
}

#[test]
fn directories_that_would_leave_the_theme_are_refused() {
    let tree = Tree::new("escape");
    tree.file(
        "data/icons/fake/index.theme",
        "[Icon Theme]\nDirectories=../other,/tmp,apps\n\
         [../other]\nSize=16\n[/tmp]\nSize=16\n[apps]\nSize=32\n",
    )
    .file("data/icons/other/app.png", "")
    .file("data/icons/fake/apps/app.png", "");
    let mut themes = tree.themes();
    let logs = capture_logs(|| {
        let found = tree.found(&mut themes, "fake", "app", 16, 1);
        assert_eq!(found.as_deref(), Some("data/icons/fake/apps/app.png"));
    });
    assert!(logs.contains(r#"directory="../other""#), "{logs}");
    assert!(logs.contains(r#"directory="/tmp""#), "{logs}");
}
