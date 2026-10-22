//! S25: `Close All` and `Close Others` must finish when a tab is answered `Don't save`.
//!
//! Reported as: answering `Don't save` re-raises the same prompt forever and nothing closes, which
//! leaves the application impossible to quit. Confirmed: three modified tabs, eight rounds of
//! `Don't save`, and the tab list never changed by a single entry.
//!
//! # Why nothing ever closed
//!
//! A batch prompt stores the *batch* op as the thing to do when answered, and `close_many` stores
//! the same value again in `pending_batch`. So `resume` received `op == batch == CloseAll` and ran
//! `perform(op)` -- which re-enters `close_many`, walks the target set from the top, and finds the
//! first modified document. That is still the document the user just answered, so it raises the
//! prompt again and returns without removing anything. The answered tab was never closed and the
//! second `perform` never ran.
//!
//! The `target` the prompt named was read for `Exit` and thrown away for every batch case. That is
//! the whole bug: for `Exit` the code says "close the document that was asked about, then continue",
//! and for `Close All` it says "re-run the batch", which is a different thing.
//!
//! # Why no existing test caught it
//!
//! `tabs_s8` drives the multi-prompt batch and answers `Save` every time. Saving clears the
//! modified bit, so the next pass skips that tab and the batch makes progress by accident. The
//! defect needs `Don't save`, where the tab stays modified and the batch keeps finding it.
//!
//! # The second bug this exposed
//!
//! With the answered tab finally closing, `Close Others` then closed the tab the user had chosen to
//! *keep*. `CloseOthers(keep)` records an index into the list as it stood when the prompt was
//! raised, and a batch removes tabs before finishing, so `keep` silently re-points: with
//! `[f0, f1, f2]` all modified and `f1` chosen to stay, answering for `f0` and re-running
//! `CloseOthers(1)` means "keep f2". Indices that outlive a removal have to be re-based, which is
//! `tabs::shift_after_removal`.

use std::fs;
use std::path::{Path, PathBuf};

use np_core::{Config, Document};
use np_tui::app::App;
use np_tui::{Action, ConfirmButton};

const W: u16 = 100;
const H: u16 = 30;

/// A fresh app whose tabs are `f0.txt .. f{n-1}.txt`, modified per `mods`.
fn app_with(dir: &Path, mods: &[bool]) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.documents.clear();
    app.active = 0;
    for (i, &modified) in mods.iter().enumerate() {
        let path = dir.join(format!("f{i}.txt"));
        fs::write(&path, format!("body {i}\n")).expect("write");
        let loaded = np_core::load(&path, None).expect("load");
        let mut doc = Document::from_loaded(&loaded);
        doc.modified = modified;
        app.open_document(doc);
    }
    app
}

fn names(app: &App) -> Vec<String> {
    app.documents.iter().map(|d| d.display_name()).collect()
}

/// Answer prompts until they stop, at most `cap` rounds. Returns the number of answers given.
///
/// The cap is the test: a correct implementation terminates in one round per modified document, so
/// a loop that never ends is reported as a failure rather than hanging the suite.
fn answer_all(app: &mut App, button: ConfirmButton, cap: usize) -> usize {
    let mut rounds = 0;
    while app.confirm.is_some() && rounds < cap {
        app.resolve_confirm(button);
        rounds += 1;
    }
    rounds
}

fn paths(app: &App) -> Vec<Option<PathBuf>> {
    app.documents.iter().map(|d| d.path.clone()).collect()
}

// --------------------------------------------------------- Close All: it must terminate

#[test]
fn close_all_with_one_modified_tab_finishes_in_one_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true]);
    app.execute(Action::CloseAllTabs);
    assert!(app.confirm.is_some(), "a modified tab must ask");

    let rounds = answer_all(&mut app, ConfirmButton::DontSave, 5);
    assert_eq!(rounds, 1, "one modified tab is one question, got {rounds}");
    assert!(
        app.confirm.is_none(),
        "the prompt must not survive the answer"
    );
}

#[test]
fn close_all_with_three_modified_tabs_asks_once_per_tab_and_finishes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true, true]);
    app.execute(Action::CloseAllTabs);

    let rounds = answer_all(&mut app, ConfirmButton::DontSave, 8);
    assert_eq!(
        rounds, 3,
        "three modified tabs is three questions, got {rounds}"
    );
    assert!(app.confirm.is_none(), "no prompt left outstanding");
    assert_eq!(
        app.documents.len(),
        1,
        "the zero-document rule leaves one fresh untitled tab: {:?}",
        names(&app)
    );
    assert!(
        app.documents[0].path.is_none(),
        "the survivor is the untitled replacement, not a file: {:?}",
        paths(&app)
    );
}

#[test]
fn each_answer_actually_closes_a_tab() {
    // The symptom the user described was not a loop of prompts but a loop of prompts in which
    // nothing ever happened. Assert the list shrinks on every single answer, which is what
    // "answering does something" means.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true, true]);
    app.execute(Action::CloseAllTabs);

    // Counting *file* tabs rather than tabs: the last answer closes the final file tab and the
    // zero-document rule immediately inserts a fresh untitled one, so the raw count bottoms out at
    // 1 and an assertion on it would read a correct final answer as doing nothing.
    let file_tabs = |a: &App| a.documents.iter().filter(|d| d.path.is_some()).count();
    let mut previous = file_tabs(&app);
    assert_eq!(previous, 3, "setup: three file tabs");
    for round in 0..3 {
        assert!(app.confirm.is_some(), "round {round}: expected a prompt");
        app.resolve_confirm(ConfirmButton::DontSave);
        let now = file_tabs(&app);
        assert!(
            now < previous,
            "round {round} closed nothing: {previous} -> {now} ({:?})",
            names(&app)
        );
        previous = now;
    }
    assert_eq!(previous, 0, "every file tab was closed by an answer");
}

#[test]
fn the_prompts_name_a_different_tab_each_time() {
    // A prompt that names the same file every round is the visible face of the defect. Each round
    // must be about a tab that has not been answered for yet.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true, true]);
    app.execute(Action::CloseAllTabs);

    let mut asked: Vec<String> = Vec::new();
    for _ in 0..3 {
        let prompt = app
            .confirm
            .as_ref()
            .map(|c| c.prompt.clone())
            .expect("a prompt is open");
        assert!(
            !asked.contains(&prompt),
            "the same question was asked twice: {asked:?} then {prompt:?}"
        );
        asked.push(prompt);
        app.resolve_confirm(ConfirmButton::DontSave);
    }
    assert_eq!(asked.len(), 3);
}

#[test]
fn a_mixed_set_asks_only_about_modified_tabs() {
    // Clean tabs close silently mid-batch; only modified ones are asked about. Two modified out of
    // four means two prompts, not four, and not zero.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[false, true, false, true]);
    app.execute(Action::CloseAllTabs);

    let rounds = answer_all(&mut app, ConfirmButton::DontSave, 8);
    assert_eq!(
        rounds, 2,
        "only the two modified tabs are asked about, got {rounds}"
    );
    assert_eq!(
        app.documents.len(),
        1,
        "everything else closed: {:?}",
        names(&app)
    );
}

// ------------------------------------------------------------------ Cancel is still a wall

#[test]
fn cancelling_mid_batch_closes_no_further_tabs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true, true]);
    app.execute(Action::CloseAllTabs);
    app.resolve_confirm(ConfirmButton::DontSave);
    assert_eq!(app.documents.len(), 2, "one closed by the first answer");

    app.resolve_confirm(ConfirmButton::Cancel);
    assert_eq!(
        app.documents.len(),
        2,
        "Cancel must not close anything further: {:?}",
        names(&app)
    );
    assert!(app.confirm.is_none(), "Cancel dismisses the prompt");

    // A stray answer with nothing pending must be ignored rather than close something.
    app.resolve_confirm(ConfirmButton::DontSave);
    assert_eq!(
        app.documents.len(),
        2,
        "a stray answer is inert: {:?}",
        names(&app)
    );
}

// ----------------------------------------------------------- Close Others keeps the kept tab

#[test]
fn close_others_with_modified_tabs_keeps_the_active_one() {
    // The second defect. With the answered tab now really closing, `CloseOthers(keep)` had its
    // index invalidated by the first removal and went on to close the tab the user chose to keep.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true, true]);
    app.active = 1;
    app.execute(Action::CloseOtherTabs);

    let rounds = answer_all(&mut app, ConfirmButton::DontSave, 8);
    assert_eq!(rounds, 2, "f0 and f2 are asked about, got {rounds}");
    assert_eq!(
        names(&app),
        vec!["f1.txt".to_string()],
        "f1 was the tab to keep and must be the only survivor"
    );
}

#[test]
fn close_others_keeps_the_active_one_when_it_is_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[false, false, true]);
    app.active = 0;
    app.execute(Action::CloseOtherTabs);

    let rounds = answer_all(&mut app, ConfirmButton::DontSave, 8);
    assert_eq!(rounds, 1, "only f2 is modified, got {rounds}");
    assert_eq!(names(&app), vec!["f0.txt".to_string()]);
}

#[test]
fn close_others_saves_before_closing_when_asked_to() {
    // The `Save` answer must still write the file it closes, in the batch path.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[false, true, false]);
    app.active = 0;
    // Put content in the modified tab so the save has something to write.
    app.documents[1].modified = true;
    app.execute(Action::CloseOtherTabs);
    assert!(
        app.confirm.is_some(),
        "the modified other tab is asked about"
    );
    app.resolve_confirm(ConfirmButton::Save);

    assert_eq!(names(&app), vec!["f0.txt".to_string()]);
    assert!(
        dir.path().join("f1.txt").exists(),
        "answering Save must leave the file on disk"
    );
}

// ------------------------------------------------------------ single close still behaves

#[test]
fn closing_one_modified_tab_closes_that_tab_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true]);
    app.active = 0;
    app.execute(Action::CloseTab);
    assert_eq!(
        app.confirm.as_ref().and_then(|c| c.target),
        Some(0),
        "the prompt is about the active tab"
    );
    app.resolve_confirm(ConfirmButton::DontSave);
    assert_eq!(names(&app), vec!["f1.txt".to_string()]);
    assert_eq!(app.active, 0);
    assert!(app.confirm.is_none(), "a single close asks once");
}

// --------------------------------------------------------------------- Exit is unaffected

#[test]
fn exit_with_modified_tabs_finishes_on_dont_save() {
    // The `Exit` arm already had the shape this stage gave the batch arms, so it should have been
    // fine. Asserted because it shares `resume` with everything above.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true]);
    app.execute(Action::Exit);

    let mut rounds = 0;
    while app.confirm.is_some() && rounds < 6 && !app.quit_requested {
        app.resolve_confirm(ConfirmButton::DontSave);
        rounds += 1;
    }
    assert!(
        app.quit_requested,
        "exit must complete after the tabs are answered"
    );
    assert_eq!(rounds, 2, "two modified tabs, two answers, got {rounds}");
}

#[test]
fn cancelling_exit_leaves_everything_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with(dir.path(), &[true, true]);
    app.execute(Action::Exit);
    app.resolve_confirm(ConfirmButton::Cancel);
    assert!(!app.quit_requested, "Cancel must not quit");
    assert_eq!(app.documents.len(), 2, "nothing closed: {:?}", names(&app));
}
