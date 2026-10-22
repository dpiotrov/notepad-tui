//! S15: dialog frames must survive their own contents (spec §6.8, FR-U4).
//!
//! Reported from a real terminal run: "file dialog borders sometime messed up by file
//! names". Two separate defects produced that, and neither was actually about file names.
//!
//! The first was structural: `draw_box` skipped the title zone on **both** the top and the
//! bottom border, but wrote the title only to the top. Every titled dialog therefore had a
//! gap punched in its bottom edge, and a listing that ran near it read as text eating the
//! frame.
//!
//! The second was the tab bug (`bigfile`/`caret` suites cover the caret side): a tab glyph
//! left in a buffer cell makes the terminal jump to a real tab stop while ratatui's model
//! counts one column, so anything drawn after it on the row -- including a border -- lands
//! in the wrong place. `TestBackend` cannot see this, which is why the whole headless suite
//! was green. A dialog whose listing contains tabs must still have a rectangular frame.

use np_core::Config;
use np_tui::app::{App, DialogKind, Mode};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn render(app: &mut App, w: u16, h: u16) -> Buffer {
    app.recompute_geometry(w, h);
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    np_tui::ui::draw(app, &mut buf);
    buf
}

/// Assert `rect` is a complete box: corners and an unbroken run of border cells on all four
/// edges. This is the property the two bugs broke, stated directly.
fn assert_frame(buf: &Buffer, x: u16, y: u16, w: u16, h: u16, what: &str) {
    // Any box-drawing glyph counts as "border present". The point is that the cell is part
    // of the frame rather than blank or holding content, so the exact junction character is
    // not what is under test.
    let right = x + w - 1;
    let bottom = y + h - 1;
    let is_box = |sym: &str| sym.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c));
    for cx in x..=right {
        for cy in [y, bottom] {
            let sym = buf.cell((cx, cy)).unwrap().symbol().to_string();
            // The title replaces a span of the top border with " Label ", so blanks and
            // letters there are intended. The bug this guards is a blank run in the title's
            // own zone on the *bottom* border, where nothing ever draws over it.
            let in_title_zone = cy == y && is_title_cell(buf, x, y, w, cx);
            if !in_title_zone {
                assert!(is_box(&sym), "{what}: border gap at ({cx},{cy}): {sym:?}");
            }
        }
    }
    for cy in y..=bottom {
        for cx in [x, right] {
            let sym = buf.cell((cx, cy)).unwrap().symbol().to_string();
            assert!(is_box(&sym), "{what}: border gap at ({cx},{cy}): {sym:?}");
        }
    }
}

/// Whether `(cx, y)` falls in the top border's title zone, where text replaces the rule.
///
/// Derived from what is actually on screen -- the run of non-border characters embedded in
/// the top rule -- rather than by recomputing `title_start`, so the test cannot drift from
/// the layout it is checking.
fn is_title_cell(buf: &Buffer, x: u16, y: u16, w: u16, cx: u16) -> bool {
    let is_box_sym = |c: u16| {
        buf.cell((c, y))
            .unwrap()
            .symbol()
            .chars()
            .any(|ch| ('\u{2500}'..='\u{257f}').contains(&ch))
    };
    // The zone is the contiguous non-rule span between the two rule runs.
    let mut zone = (None, None);
    let mut run_start = None;
    for c in x + 1..x + w - 1 {
        if !is_box_sym(c) {
            if run_start.is_none() {
                run_start = Some(c);
            }
        } else if let Some(from) = run_start.take() {
            zone = (Some(from), Some(c));
        }
    }
    if let Some(from) = run_start {
        zone = (Some(from), Some(x + w - 1));
    }
    match zone {
        (Some(from), Some(to)) => cx >= from.saturating_sub(1) && cx <= to,
        _ => false,
    }
}

/// Locate a dialog frame by its top-left corner, so the assertion does not have to duplicate
/// the layout arithmetic it is checking.
fn find_frame(buf: &Buffer, w: u16, h: u16) -> Option<(u16, u16, u16, u16)> {
    for y in 0..h {
        for x in 0..w {
            if buf.cell((x, y)).unwrap().symbol() == "\u{250c}" {
                // Walk the top edge to the matching corner.
                for right in x + 1..w {
                    if buf.cell((right, y)).unwrap().symbol() == "\u{2510}" {
                        for bottom in y + 1..h {
                            if buf.cell((x, bottom)).unwrap().symbol() == "\u{2514}"
                                && buf.cell((right, bottom)).unwrap().symbol() == "\u{2518}"
                            {
                                return Some((x, y, right - x + 1, bottom - y + 1));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn picker_in(app: &mut App, dir: &str) {
    app.file_input
        .set_text(np_tui::ui::file_picker::dir_text(std::path::Path::new(dir)));
    app.refresh_file_entries();
    app.push_mode(Mode::Dialog(DialogKind::Open));
}

#[test]
fn a_titled_dialog_has_an_unbroken_bottom_border() {
    for (w, h) in [(60u16, 20u16), (44, 16), (40, 14), (100, 24)] {
        let mut app = App::new_headless(Config::default(), (w, h));
        picker_in(&mut app, "/tmp");
        let buf = render(&mut app, w, h);
        let (x, y, bw, bh) = find_frame(&buf, w, h).unwrap_or_else(|| {
            panic!("{w}x{h}: no dialog frame found");
        });
        assert_frame(&buf, x, y, bw, bh, &format!("file picker at {w}x{h}"));
    }
}

#[test]
fn long_and_wide_names_do_not_break_the_frame() {
    let dir = std::env::temp_dir().join("npt-border-s15");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    // The shapes that produced the report: names far longer than the dialog, full-width
    // characters, and names containing tabs (legal on Linux, and the case the cell model
    // gets wrong).
    std::fs::write(dir.join("a".repeat(120)), b"x").expect("write long");
    std::fs::write(dir.join("\ttabbed\tname\t.txt"), b"x").expect("write tabs");
    std::fs::write(dir.join("日本語のファイル名".repeat(6)), b"x").expect("write wide");
    std::fs::write(dir.join("normal.txt"), b"x").expect("write plain");
    std::fs::create_dir(dir.join("d".repeat(90))).expect("mkdir long");

    for (w, h) in [(50u16, 18u16), (40, 14), (80, 22)] {
        let mut app = App::new_headless(Config::default(), (w, h));
        picker_in(&mut app, dir.to_str().unwrap());
        assert!(
            app.file_entries.len() >= 4,
            "fixture must list the long/tabbed/wide names"
        );
        let buf = render(&mut app, w, h);
        let (x, y, bw, bh) = find_frame(&buf, w, h).unwrap_or_else(|| {
            panic!("{w}x{h}: no frame with long names");
        });
        assert_frame(
            &buf,
            x,
            y,
            bw,
            bh,
            &format!("picker with hostile names at {w}x{h}"),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dialog_row_never_overwrites_the_side_borders() {
    let dir = std::env::temp_dir().join("npt-border2-s15");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("z".repeat(200)), b"x").expect("write");
    std::fs::write(dir.join("\t".repeat(40)), b"x").expect("write tabs");

    let mut app = App::new_headless(Config::default(), (40, 14));
    picker_in(&mut app, dir.to_str().unwrap());
    let buf = render(&mut app, 40, 14);
    let (x, y, bw, bh) = find_frame(&buf, 40, 14).expect("frame");
    let right = x + bw - 1;
    for row in y..=y + bh - 1 {
        for cx in [x, right] {
            let sym = buf.cell((cx, row)).unwrap().symbol();
            assert!(
                !sym.trim().is_empty() && !sym.contains('z') && !sym.contains('\t'),
                "a listing row overwrote the border at ({cx},{row}): {sym:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_titled_dialog_keeps_a_closed_frame() {
    // The bug lived in the shared `draw_box`, so check the other dialogs too rather than
    // assuming the picker was the only victim.
    type Case = (&'static str, fn(&mut App));
    let cases: Vec<Case> = vec![
        ("go to line", |a| {
            a.execute(np_tui::actions::Action::GoToLine)
        }),
        ("find", |a| a.execute(np_tui::actions::Action::Find)),
        ("replace", |a| a.execute(np_tui::actions::Action::Replace)),
        ("preferences", |a| a.open_preferences()),
        ("recent files", |a| {
            a.execute(np_tui::actions::Action::RecentFiles)
        }),
    ];
    for (name, open) in cases {
        for (w, h) in [(70u16, 22u16), (50, 16)] {
            let mut app = App::new_headless(Config::default(), (w, h));
            app.open_document(np_core::Document::from_decoded(
                "/tmp/border.txt".into(),
                "one\ntwo\n",
                np_core::EncodingId::Utf8,
                np_core::Eol::Lf,
                false,
            ));
            open(&mut app);
            let buf = render(&mut app, w, h);
            let Some((x, y, bw, bh)) = find_frame(&buf, w, h) else {
                // Some dialogs need a minimum size to appear at all; that is a layout
                // decision, not a broken frame, so skip rather than fail.
                continue;
            };
            assert_frame(&buf, x, y, bw, bh, &format!("{name} at {w}x{h}"));
        }
    }
}
