//! Pull requests, as docs/tui-design.md §2.4 and the TUI's PR rows show them: each PR on
//! one row, its number in its state's colour and its title, then its state, checks, review,
//! mergeability and head branch. Live PRs (open and draft) come first; checks and reviews
//! matter only for them.
//!
//! A row opens its PR in the browser; its menu also copies the link and, for a session that
//! can be driven from here, unlinks it after asking. The session view lists its session's PRs
//! over the transcript ([`strip`]); the window lists every session's ([`session_group`]).

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use herder_protocol::{CiStatus, Mergeable, PrState, PullRequest, ReviewStatus};

use crate::transcript::{hbox, line_label, vbox};

/// Unlinks the PR with this number from its session.
pub type Unlink = Rc<dyn Fn(u64)>;

/// Pixels the session view's PR list grows to before it scrolls: three rows.
const STRIP_HEIGHT: i32 = 200;

/// The PR number `text` names: `123`, `#123`, or a pull request URL, as the TUI reads it.
pub fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = match text.split_once("/pull/") {
        Some((_, rest)) => rest.split(['/', '#', '?']).next().unwrap_or(rest),
        None => text.strip_prefix('#').unwrap_or(text),
    };
    digits.parse().ok().filter(|number| *number > 0)
}

/// Whether the PR is still open, as a draft or not.
pub fn live(pr: &PullRequest) -> bool {
    matches!(pr.state, PrState::Open | PrState::Draft)
}

/// `prs`, live ones first, each group in link order.
pub fn ordered(prs: &[PullRequest]) -> Vec<PullRequest> {
    let mut prs = prs.to_vec();
    prs.sort_by_key(|pr| !live(pr));
    prs
}

/// A state's word and style class.
pub fn state(state: PrState) -> (&'static str, &'static str) {
    match state {
        PrState::Open => ("open", "pr-open"),
        PrState::Draft => ("draft", "pr-draft"),
        PrState::Merged => ("merged", "pr-merged"),
        PrState::Closed => ("closed", "pr-closed"),
    }
}

/// A live PR's checks, review and mergeability, as words with their style class; the review
/// only once one is asked for or given.
pub fn checks(pr: &PullRequest) -> Vec<(&'static str, &'static str)> {
    if !live(pr) {
        return Vec::new();
    }
    let mut checks = Vec::new();
    checks.push(match pr.ci {
        CiStatus::Passing => ("✓ ci", "ok"),
        CiStatus::Failing => ("✗ ci", "bad"),
        CiStatus::Pending => ("… ci", "wait"),
        CiStatus::None => ("– ci", "unknown"),
    });
    match pr.review {
        ReviewStatus::Approved => checks.push(("✓ approved", "ok")),
        ReviewStatus::ChangesRequested => checks.push(("✗ changes", "bad")),
        ReviewStatus::Required => checks.push(("… review", "wait")),
        ReviewStatus::None => {}
    }
    checks.push(match pr.mergeable {
        Mergeable::Clean => ("✓ merge", "ok"),
        Mergeable::Conflicting => ("✗ conflict", "bad"),
        Mergeable::Unknown => ("? merge", "unknown"),
    });
    checks
}

/// How much of a PR its row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// `#12 Title` over its state, checks and head branch.
    Full,
    /// As `Full`, without the branch: for a narrow window.
    Narrow,
    /// One line, as the TUI's compact strip: `#12 ✓ ! Title`, the checks' mark and a `!`
    /// for a conflict or requested changes.
    Line,
}

/// One PR as a list row. Activating it opens the PR in the browser.
pub fn row(pr: &PullRequest, size: Size, unlink: Option<Unlink>) -> gtk::ListBoxRow {
    let (word, class) = state(pr.state);
    let number = gtk::Label::builder()
        .label(format!("#{}", pr.number))
        .valign(gtk::Align::Start)
        .css_classes(["pr-number", class])
        .build();
    let title = line_label(&pr.title);
    title.set_hexpand(true);
    title.add_css_class("pr-title");
    let top = hbox(8);
    top.append(&number);
    if size == Size::Line && live(pr) {
        let (mark, check) = match pr.ci {
            CiStatus::Passing => ("✓", "ok"),
            CiStatus::Failing => ("✗", "bad"),
            CiStatus::Pending => ("…", "wait"),
            CiStatus::None => ("–", "unknown"),
        };
        let mark = gtk::Label::builder()
            .label(mark)
            .valign(gtk::Align::Start)
            .tooltip_text(checks(pr).first().map_or("", |(text, _)| *text))
            .css_classes(["pr-check", check])
            .build();
        top.append(&mark);
        if pr.mergeable == Mergeable::Conflicting || pr.review == ReviewStatus::ChangesRequested {
            top.append(
                &gtk::Label::builder()
                    .label("!")
                    .valign(gtk::Align::Start)
                    .tooltip_text("A conflict or requested changes")
                    .css_classes(["pr-check", "bad"])
                    .build(),
            );
        }
    }
    top.append(&title);

    let meta = hbox(10);
    meta.add_css_class("pr-meta");
    meta.append(
        &gtk::Label::builder()
            .label(word)
            .valign(gtk::Align::Center)
            .css_classes(["pr-state", class])
            .build(),
    );
    for (text, class) in checks(pr) {
        meta.append(
            &gtk::Label::builder()
                .label(text)
                .css_classes(["pr-check", class])
                .build(),
        );
    }
    if let Some(branch) = pr.head_branch.as_deref().filter(|_| size == Size::Full) {
        let branch = line_label(branch);
        branch.add_css_class("pr-branch");
        meta.append(&branch);
    }
    let text = vbox(4);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    text.append(&top);
    if size != Size::Line {
        text.append(&meta);
    }

    let menu = gio::Menu::new();
    menu.append(Some("Open in Browser"), Some("pr.open"));
    menu.append(Some("Copy Link"), Some("pr.copy"));
    if unlink.is_some() {
        let section = gio::Menu::new();
        section.append(Some("Unlink…"), Some("pr.unlink"));
        menu.append_section(None, &section);
    }
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .tooltip_text("More")
        .css_classes(["flat", "circular"])
        .build();

    let content = hbox(12);
    content.add_css_class("pr-row");
    if size == Size::Line {
        content.add_css_class("line");
    }
    content.append(&text);
    content.append(&more);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(true)
        .action_name("pr.open")
        .tooltip_text(format!("Open {} in the browser", pr.url))
        .build();

    let actions = gio::SimpleActionGroup::new();
    let open = gio::SimpleAction::new("open", None);
    let url = pr.url.clone();
    let opener = row.downgrade();
    open.connect_activate(move |_, _| {
        if let Some(row) = opener.upgrade() {
            open_url(&row, &url);
        }
    });
    actions.add_action(&open);
    let copy = gio::SimpleAction::new("copy", None);
    let url = pr.url.clone();
    let copier = row.downgrade();
    copy.connect_activate(move |_, _| {
        if let Some(row) = copier.upgrade() {
            row.clipboard().set_text(&url);
        }
    });
    actions.add_action(&copy);
    if let Some(unlink) = unlink {
        let action = gio::SimpleAction::new("unlink", None);
        let number = pr.number;
        let asker = row.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(row) = asker.upgrade() {
                confirm_unlink(&row, number, Rc::clone(&unlink));
            }
        });
        actions.add_action(&action);
    }
    row.insert_action_group("pr", Some(&actions));
    row
}

/// The session view's list of its session's PRs, live first; it scrolls past three rows.
/// Narrow, each is one line.
pub fn strip(prs: &[PullRequest], compact: bool, unlink: Option<Unlink>) -> gtk::Widget {
    let size = if compact { Size::Line } else { Size::Full };
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for pr in ordered(prs) {
        list.append(&row(&pr, size, unlink.clone()));
    }
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(STRIP_HEIGHT)
        .child(&list)
        .build()
        .upcast()
}

/// What the window's PR list shows of a session over its PRs.
pub struct SessionPrs<'a> {
    pub title: &'a str,
    /// The session's state, as words, and its style class.
    pub status: (&'a str, &'a str),
    /// Where it runs.
    pub place: &'a str,
    pub prs: &'a [PullRequest],
    /// For a session that can be driven from here.
    pub unlink: Option<Unlink>,
    pub open: Rc<dyn Fn()>,
}

/// A session's PRs as a group: its title, state and place over its PRs, and a button that
/// opens it.
pub fn session_group(session: &SessionPrs, compact: bool) -> adw::PreferencesGroup {
    let (status, _) = session.status;
    let description = [status, session.place]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let group = adw::PreferencesGroup::builder()
        .title(glib::markup_escape_text(session.title))
        .description(glib::markup_escape_text(&description))
        .build();
    let open = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Open the session")
        .css_classes(["flat", "circular"])
        .build();
    let opener = Rc::clone(&session.open);
    open.connect_clicked(move |_| opener());
    group.set_header_suffix(Some(&open));
    let size = if compact { Size::Narrow } else { Size::Full };
    for pr in session.prs {
        group.add(&row(pr, size, session.unlink.clone()));
    }
    group
}

/// Asks before unlinking PR `number`, as the TUI's confirm does for destructive actions.
fn confirm_unlink(parent: &impl IsA<gtk::Widget>, number: u64, unlink: Unlink) {
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Unlink #{number}?"))
        .body(
            "herder stops tracking this pull request for the session. \
             It can be linked again by its number.",
        )
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("unlink", "Unlink")]);
    dialog.set_response_appearance("unlink", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |_, response| {
        if response == "unlink" {
            unlink(number);
        }
    });
    dialog.present(Some(parent));
}

/// Asks for a PR to link, by its number or link, and links it.
pub fn link_dialog(parent: &impl IsA<gtk::Widget>, title: &str, link: impl Fn(u64) + 'static) {
    let entry = gtk::Entry::builder()
        .placeholder_text("123, #123 or a link")
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading("Link a Pull Request")
        .body(format!(
            "To {title}, by its number in the session's repository or by its link."
        ))
        .extra_child(&entry)
        .close_response("cancel")
        .default_response("link")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("link", "Link")]);
    dialog.set_response_appearance("link", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("link", false);
    let checked = dialog.clone();
    entry.connect_changed(move |entry| {
        checked.set_response_enabled("link", parse_number(&entry.text()).is_some());
    });
    let typed = entry.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "link"
            && let Some(number) = parse_number(&typed.text())
        {
            link(number);
        }
    });
    dialog.present(Some(parent));
    entry.grab_focus();
}

/// A pull request's icon, which the icon theme has none of: two commits on a line, and a
/// third whose line bends back into it. Drawn in the widget's colour, as a symbolic icon is.
pub fn icon() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(|area, cr, width, height| {
        let color = area.color();
        cr.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            f64::from(color.alpha()),
        );
        // A 16 px grid, centred.
        cr.translate(f64::from(width - 16) / 2.0, f64::from(height - 16) / 2.0);
        cr.set_line_width(1.5);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_join(gtk::cairo::LineJoin::Round);
        let ring = |x: f64, y: f64| {
            cr.new_sub_path();
            cr.arc(x, y, 1.9, 0.0, std::f64::consts::TAU);
        };
        ring(4.0, 3.5);
        ring(4.0, 12.5);
        ring(12.0, 12.5);
        cr.move_to(4.0, 5.4);
        cr.line_to(4.0, 10.6);
        // From the third commit up, and back left to an arrow's head.
        cr.move_to(12.0, 10.6);
        cr.line_to(12.0, 6.5);
        cr.curve_to(12.0, 4.6, 11.0, 3.5, 9.0, 3.5);
        cr.line_to(7.0, 3.5);
        cr.move_to(8.6, 1.9);
        cr.line_to(7.0, 3.5);
        cr.line_to(8.6, 5.1);
        // Painting is best effort: a failed stroke leaves the icon blank.
        let _ = cr.stroke();
    });
    area
}

#[cfg(not(test))]
fn open_url(widget: &gtk::ListBoxRow, url: &str) {
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(url).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
}

#[cfg(test)]
thread_local! {
    /// The links the tests' rows opened, in place of a browser.
    pub static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn open_url(_: &gtk::ListBoxRow, url: &str) {
    OPENED.with(|opened| opened.borrow_mut().push(url.to_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lists::tests::pr;

    #[test]
    fn numbers_and_links_name_a_pull_request() {
        assert_eq!(parse_number("42"), Some(42));
        assert_eq!(parse_number(" #42 "), Some(42));
        assert_eq!(
            parse_number("https://github.com/acme/app/pull/42/files#diff"),
            Some(42)
        );
        assert_eq!(parse_number("github.com/acme/app/issues/42"), None);
        assert_eq!(parse_number("0"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn checks_read_as_the_tuis_and_only_for_live_prs() {
        let mut open = pr(12, PrState::Open, CiStatus::Passing);
        open.review = ReviewStatus::ChangesRequested;
        open.mergeable = Mergeable::Conflicting;
        assert_eq!(
            checks(&open),
            [("✓ ci", "ok"), ("✗ changes", "bad"), ("✗ conflict", "bad")]
        );
        let draft = pr(9, PrState::Draft, CiStatus::Pending);
        assert_eq!(checks(&draft), [("… ci", "wait"), ("✓ merge", "ok")]);
        assert!(checks(&pr(7, PrState::Merged, CiStatus::Passing)).is_empty());
        let numbers: Vec<u64> = ordered(&[
            pr(7, PrState::Merged, CiStatus::None),
            pr(9, PrState::Draft, CiStatus::None),
            pr(3, PrState::Closed, CiStatus::None),
            pr(12, PrState::Open, CiStatus::None),
        ])
        .iter()
        .map(|pr| pr.number)
        .collect();
        assert_eq!(numbers, [9, 12, 7, 3]);
    }
}
