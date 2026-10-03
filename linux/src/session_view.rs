//! The session view: the open session's transcript ([`crate::transcript`]) above what waits
//! on the user or the composer, as docs/tui-design.md §2.1 lays out the chat.
//!
//! - An approval or a question replaces the composer with a card, answered with its buttons
//!   or `y` / `n` and the digits, as the TUI's request panel.
//! - The composer sends with Enter and adds a line with Shift+Enter. Its controls show and
//!   switch the session's account, model and permission mode; another provider's account
//!   replays the transcript there, as the TUI's switch picker says.
//! - While a turn runs, a status line counts its time and the composer can stop it;
//!   prompts sent meanwhile wait, marked queued, until the daemon starts them.
//! - Archived and moved sessions, and a vault's, are read-only: no composer.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, pango};
use herder_client_core::Machine;
use herder_protocol::{
    Account, Answer, ApprovalDecision, CommandBody, ItemBody, ItemId, PermissionMode, Route,
    SessionId, SessionStatus, Timestamp,
};

use crate::lists::SessionKey;
use crate::session::{
    PendingApproval, PendingQuestion, Session, duration, first_line, mode_description, mode_name,
    reason_text,
};
use crate::transcript::{self, Context, hbox, line_label, vbox, wrapped};

/// Sends a command to a machine; the error says why the daemon refused it.
pub type Sender = Rc<dyn Fn(herder_protocol::HostId, CommandBody) -> Reply>;
/// What [`Sender`] answers.
pub type Reply = Pin<Box<dyn Future<Output = Result<(), String>>>>;

/// Lines of a request's command shown before it scrolls.
const REQUEST_LINES: i32 = 15;
/// Most pixels the editor grows to before it scrolls.
const EDITOR_HEIGHT: i32 = 220;
/// Pixels from the bottom within which the transcript follows new items.
const FOLLOW: f64 = 48.0;

/// The permission modes, in the TUI's order.
const MODES: [PermissionMode; 4] = [
    PermissionMode::ReadOnly,
    PermissionMode::Ask,
    PermissionMode::AutoEdit,
    PermissionMode::FullAccess,
];

/// The session view and the widgets it redraws. Cheap to clone: every field is shared.
#[derive(Clone)]
pub struct SessionView {
    page: adw::NavigationPage,
    title: adw::WindowTitle,
    status: gtk::Label,
    toasts: adw::ToastOverlay,
    scroller: gtk::ScrolledWindow,
    /// The durable entries, one widget each.
    transcript: gtk::Box,
    /// Streaming items and queued prompts, redrawn on every update.
    streaming: gtk::Box,
    status_line: gtk::Box,
    working: gtk::Label,
    spinner: gtk::Spinner,
    usage: gtk::Label,
    /// `composer`, `request` or `read-only`.
    bottom: gtk::Stack,
    request: gtk::Box,
    read_only: gtk::Label,
    composer: Composer,
    state: Rc<RefCell<State>>,
    follow: Rc<Cell<bool>>,
}

/// The composer's widgets.
#[derive(Clone)]
struct Composer {
    text: gtk::TextView,
    placeholder: gtk::Label,
    send: gtk::Button,
    stop: gtk::Button,
    account: gtk::MenuButton,
    account_label: gtk::Label,
    accounts: gtk::Box,
    model: gtk::MenuButton,
    model_label: gtk::Label,
    model_entry: gtk::Entry,
    models: gtk::ListBox,
    mode: gtk::MenuButton,
    mode_label: gtk::Label,
    modes: gtk::ListBox,
}

/// What the view shows, as of the last update.
struct State {
    key: Option<SessionKey>,
    session: Session,
    machine_name: String,
    accounts: Vec<Account>,
    /// Why the session cannot be driven from here, if it cannot.
    read_only: Option<&'static str>,
    status: SessionStatus,
    /// Models the app's sessions of the same provider use, for the model picker.
    recent: Vec<String>,
    /// What each entry was drawn from, by entry, with its widget.
    drawn: Vec<(String, Option<gtk::Widget>)>,
    expanded: Rc<RefCell<HashSet<ItemId>>>,
    /// Prompts sent from here not in the transcript yet.
    pending: Vec<String>,
    /// The request the card shows, so a redraw keeps what the user typed.
    request: Option<String>,
    /// The request card's age label, and when the request was put.
    ages: Vec<(gtk::Label, Timestamp)>,
    compact: bool,
    sender: Option<Sender>,
    opener: Option<Rc<dyn Fn(SessionKey)>>,
}

impl SessionView {
    pub fn new() -> Self {
        let title = adw::WindowTitle::new("", "");
        let status = gtk::Label::new(None);
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_end(&status);

        let transcript = vbox(0);
        transcript.add_css_class("transcript");
        let streaming = vbox(0);
        streaming.add_css_class("transcript");
        streaming.set_margin_top(-18);
        let column = vbox(0);
        column.append(&transcript);
        column.append(&streaming);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(
                &adw::Clamp::builder()
                    .maximum_size(860)
                    .tightening_threshold(600)
                    .child(&column)
                    .build(),
            )
            .build();

        let spinner = gtk::Spinner::new();
        let working = gtk::Label::new(None);
        let usage = gtk::Label::builder()
            .hexpand(true)
            .xalign(1.0)
            .css_classes(["usage"])
            .build();
        let status_line = hbox(8);
        status_line.add_css_class("status-line");
        status_line.append(&spinner);
        status_line.append(&working);
        status_line.append(&usage);

        let composer = Composer::new();
        let request = vbox(0);
        let read_only = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["read-only"])
            .build();
        let bottom = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vhomogeneous(false)
            .interpolate_size(true)
            .build();
        bottom.add_named(&composer.frame(), Some("composer"));
        bottom.add_named(&request, Some("request"));
        bottom.add_named(&read_only, Some("read-only"));
        let bottom_column = vbox(0);
        bottom_column.add_css_class("bottom");
        bottom_column.append(&status_line);
        bottom_column.append(&bottom);

        let body = vbox(0);
        body.append(&scroller);
        body.append(
            &adw::Clamp::builder()
                .maximum_size(860)
                .tightening_threshold(600)
                .child(&bottom_column)
                .build(),
        );
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&body));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        let page = adw::NavigationPage::builder()
            .title("Session")
            .tag("session")
            .child(&view)
            .build();

        let this = Self {
            page,
            title,
            status,
            toasts,
            scroller,
            transcript,
            streaming,
            status_line,
            working,
            spinner,
            usage,
            bottom,
            request,
            read_only,
            composer,
            state: Rc::new(RefCell::new(State {
                key: None,
                session: Session::default(),
                machine_name: String::new(),
                accounts: Vec::new(),
                read_only: None,
                status: SessionStatus::Unknown,
                recent: Vec::new(),
                drawn: Vec::new(),
                expanded: Rc::default(),
                pending: Vec::new(),
                request: None,
                ages: Vec::new(),
                compact: false,
                sender: None,
                opener: None,
            })),
            follow: Rc::new(Cell::new(true)),
        };
        this.wire();
        this
    }

    pub fn page(&self) -> &adw::NavigationPage {
        &self.page
    }

    /// The session shown, if any.
    pub fn key(&self) -> Option<SessionKey> {
        self.state.borrow().key.clone()
    }

    /// Where commands go.
    pub fn set_sender(&self, sender: Sender) {
        self.state.borrow_mut().sender = Some(sender);
    }

    /// What opens another session, from a task line.
    pub fn set_opener(&self, opener: Rc<dyn Fn(SessionKey)>) {
        self.state.borrow_mut().opener = Some(opener);
    }

    /// Narrow: the composer's controls show less, and the header says what they would.
    pub fn set_compact(&self, compact: bool) {
        self.state.borrow_mut().compact = compact;
        self.redraw_chrome();
    }

    /// Forgets the session shown, which is gone.
    pub fn close(&self) {
        let mut state = self.state.borrow_mut();
        state.key = None;
        state.session = Session::default();
    }

    /// Shows `session` of `machine` as `key`, keeping what is drawn when it is the session
    /// shown already. `recent` are the models to offer.
    pub fn show(
        &self,
        key: &SessionKey,
        session: &Session,
        machine: &Machine,
        recent: Vec<String>,
    ) {
        {
            let mut state = self.state.borrow_mut();
            if state.key.as_ref() != Some(key) {
                state.key = Some(key.clone());
                state.drawn.clear();
                state.pending.clear();
                state.request = None;
                state.expanded.borrow_mut().clear();
                while let Some(child) = self.transcript.first_child() {
                    self.transcript.remove(&child);
                }
                self.follow.set(true);
            }
            let head = machine
                .sessions
                .iter()
                .find(|head| head.session_id == key.session_id);
            state.status = head.map_or(session.status, |head| head.status);
            state.read_only = if !machine.hosts.is_empty() {
                Some(
                    "A vault's sessions are read-only here: open the session from its own machine to drive it.",
                )
            } else {
                match state.status {
                    SessionStatus::Archived => Some("This session is archived and read-only."),
                    SessionStatus::Moved => {
                        Some("This session moved to another host and is read-only here.")
                    }
                    _ => None,
                }
            };
            state.machine_name.clone_from(&machine.name);
            state.accounts.clone_from(&machine.accounts);
            state.recent = recent;
            // Prompts that joined the transcript are no longer pending.
            for entry in &session.entries[state.session.entries.len().min(session.entries.len())..]
            {
                if let crate::session::Entry::Item(item) = entry
                    && let ItemBody::UserMessage { text } = &item.body
                    && let Some(at) = state.pending.iter().position(|p| p == text)
                {
                    state.pending.remove(at);
                }
            }
            state.session = session.clone();
        }
        self.redraw_transcript();
        self.redraw_streaming();
        self.redraw_bottom();
        self.redraw_chrome();
        self.tick();
        if self.follow.get() {
            // Once laid out: the new items' size is not known yet.
            let adjustment = self.scroller.vadjustment();
            glib::idle_add_local_once(move || {
                adjustment.set_value(adjustment.upper() - adjustment.page_size());
            });
        }
    }

    fn context<'a>(state: &'a State, opener: &'a Rc<dyn Fn(SessionId)>) -> Context<'a> {
        Context {
            session: &state.session,
            expanded: &state.expanded,
            open_child: opener,
        }
    }

    fn child_opener(&self) -> Rc<dyn Fn(SessionId)> {
        let state = Rc::clone(&self.state);
        Rc::new(move |session_id| {
            let (opener, host_id) = {
                let state = state.borrow();
                (
                    state.opener.clone(),
                    state.key.as_ref().map(|key| key.host_id.clone()),
                )
            };
            if let (Some(opener), Some(host_id)) = (opener, host_id) {
                opener(SessionKey {
                    host_id,
                    session_id,
                });
            }
        })
    }

    /// Draws new entries and redraws those whose result, approval or state changed.
    fn redraw_transcript(&self) {
        let opener = self.child_opener();
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let mut drawn = std::mem::take(&mut state.drawn);
        let cx = Self::context(state, &opener);
        let mut previous: Option<gtk::Widget> = None;
        for (at, entry) in cx.session.entries.iter().enumerate() {
            let signature = transcript::signature(cx.session, entry);
            if let Some((known, widget)) = drawn.get(at)
                && *known == signature
            {
                if widget.is_some() {
                    previous.clone_from(widget);
                }
                continue;
            }
            let widget = transcript::entry(&cx, entry);
            if let Some(widget) = &widget {
                self.transcript
                    .insert_child_after(widget, previous.as_ref());
                previous = Some(widget.clone());
            }
            match drawn.get_mut(at) {
                Some(slot) => {
                    if let Some(old) = &slot.1 {
                        self.transcript.remove(old);
                    }
                    *slot = (signature, widget);
                }
                None => drawn.push((signature, widget)),
            }
        }
        state.drawn = drawn;
    }

    /// Redraws the items streaming now and the prompts waiting to join the transcript.
    fn redraw_streaming(&self) {
        while let Some(child) = self.streaming.first_child() {
            self.streaming.remove(&child);
        }
        let opener = self.child_opener();
        let state = self.state.borrow();
        let cx = Self::context(&state, &opener);
        for item in &cx.session.streaming {
            if let Some(widget) = transcript::item(&cx, item, true) {
                self.streaming.append(&widget);
            }
        }
        let running = state.session.running();
        for prompt in &state.pending {
            let badge = if running { "QUEUED" } else { "SENDING" };
            self.streaming
                .append(&transcript::user_message(prompt, Some(badge)));
        }
        self.streaming
            .set_visible(self.streaming.first_child().is_some());
    }

    /// The card of the first request waiting, or the composer, or why there is none.
    fn redraw_bottom(&self) {
        let (approval, question, read_only) = {
            let state = self.state.borrow();
            let session = &state.session;
            // Requests put to the user first; a user can answer the primary's too.
            let approval = session
                .approvals
                .iter()
                .find(|a| a.routed_to == Route::User)
                .or(session.approvals.first())
                .cloned();
            let question = session
                .questions
                .iter()
                .find(|q| q.routed_to == Route::User)
                .or(session.questions.first())
                .cloned();
            (approval, question, state.read_only)
        };
        if let Some(why) = read_only {
            self.read_only.set_label(why);
            self.bottom.set_visible_child_name("read-only");
            return;
        }
        let id = approval
            .as_ref()
            .map(|a| a.id.to_string())
            .or_else(|| question.as_ref().map(|q| q.id.to_string()));
        let shown = self.state.borrow().request.clone();
        if id.is_some() && id == shown {
            // The same request: only its age changed, which the tick redraws.
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.request.clone_from(&id);
            state.ages.clear();
        }
        while let Some(child) = self.request.first_child() {
            self.request.remove(&child);
        }
        let card = match (approval, question) {
            (Some(approval), _) => self.approval_card(&approval),
            (None, Some(question)) => self.question_card(&question),
            (None, None) => {
                self.bottom.set_visible_child_name("composer");
                return;
            }
        };
        self.request.append(&card);
        self.bottom.set_visible_child_name("request");
    }

    fn approval_card(&self, approval: &PendingApproval) -> gtk::Box {
        let (tool, command, more) = {
            let state = self.state.borrow();
            let session = &state.session;
            let call = session.tool_call(&approval.tool_call_id);
            let tool = call.map_or_else(|| "tool".to_owned(), |(name, _)| name.to_owned());
            let command = call
                .map(|(name, input)| {
                    let (label, args) = crate::tools::summary(name, input, None, &session.worktree);
                    if crate::tools::kind(name) == crate::tools::Kind::Shell {
                        format!(
                            "$ {}",
                            crate::tools::in_worktree(
                                &crate::tools::command(input),
                                &session.worktree
                            )
                        )
                    } else {
                        format!("{label} {args}").trim().to_owned()
                    }
                })
                .filter(|command| !command.is_empty())
                .unwrap_or_else(|| approval.summary.clone());
            (tool, command, session.approvals.len() - 1)
        };
        let card = self.card("△ approval", &tool, approval.since, more);
        if command != approval.summary && !approval.summary.is_empty() {
            card.append(&wrapped(&approval.summary, true));
        }
        card.append(&command_view(&command));
        why(
            &card,
            approval.routed_to,
            approval.reason,
            approval.note.as_deref(),
        );

        let deny = gtk::Button::builder()
            .label("Deny")
            .tooltip_text("Deny (n)")
            .build();
        let allow = gtk::Button::builder()
            .label("Allow")
            .tooltip_text("Allow once (y)")
            .css_classes(["suggested-action"])
            .build();
        let buttons = hbox(9);
        let hint = gtk::Label::builder()
            .label("y allow · n deny")
            .hexpand(true)
            .xalign(0.0)
            .css_classes(["muted", "caption"])
            .build();
        buttons.append(&hint);
        buttons.append(&deny);
        buttons.append(&allow);
        card.append(&buttons);
        let answer = |decision| {
            let view = self.clone();
            let id = approval.id.clone();
            move || {
                let Some(key) = view.key() else { return };
                view.send(CommandBody::AnswerApproval {
                    session_id: key.session_id,
                    approval_id: id.clone(),
                    decision,
                });
            }
        };
        let on_allow = answer(ApprovalDecision::Allow);
        let on_deny = answer(ApprovalDecision::Deny);
        let keys = gtk::ShortcutController::new();
        keys.add_shortcut(shortcut(gdk::Key::y, on_allow.clone()));
        keys.add_shortcut(shortcut(gdk::Key::n, on_deny.clone()));
        card.add_controller(keys);
        allow.connect_clicked(move |_| on_allow());
        deny.connect_clicked(move |_| on_deny());
        let focus = allow.clone();
        glib::idle_add_local_once(move || {
            focus.grab_focus();
        });
        card
    }

    fn question_card(&self, question: &PendingQuestion) -> gtk::Box {
        let more = self.state.borrow().session.questions.len() - 1;
        let card = self.card("? question", "", question.since, more);
        card.append(&transcript::assistant(&question.text, false));
        why(
            &card,
            question.routed_to,
            question.reason,
            question.note.as_deref(),
        );
        let answer = {
            let view = self.clone();
            let id = question.id.clone();
            move |answer: Answer| {
                let Some(key) = view.key() else { return };
                view.send(CommandBody::AnswerQuestion {
                    session_id: key.session_id,
                    question_id: id.clone(),
                    answer,
                });
            }
        };
        let keys = gtk::ShortcutController::new();
        if !question.choices.is_empty() {
            let choices = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            for (at, choice) in question.choices.iter().enumerate() {
                let row = hbox(12);
                row.add_css_class("choice");
                let number = gtk::Label::builder()
                    .label((at + 1).to_string())
                    .valign(gtk::Align::Start)
                    .css_classes(["choice-number"])
                    .build();
                row.append(&number);
                row.append(&line_label(choice));
                choices.append(&row);
                let index = u32::try_from(at).unwrap_or(u32::MAX);
                if let Some(key) = digit(at + 1) {
                    let answer = answer.clone();
                    keys.add_shortcut(shortcut(key, move || answer(Answer::Choice { index })));
                }
            }
            let picked = answer.clone();
            choices.connect_row_activated(move |_, row| {
                let index = u32::try_from(row.index()).unwrap_or(u32::MAX);
                picked(Answer::Choice { index });
            });
            card.append(&choices);
        }
        let entry = gtk::Entry::builder()
            .placeholder_text(if question.choices.is_empty() {
                "Type an answer"
            } else {
                "Or type an answer"
            })
            .hexpand(true)
            .build();
        let send = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Answer")
            .css_classes(["circular", "suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        let row = hbox(9);
        row.append(&entry);
        row.append(&send);
        card.append(&row);
        let typed = {
            let entry = entry.clone();
            move || {
                let text = entry.text().trim().to_owned();
                if !text.is_empty() {
                    answer(Answer::Text { text });
                }
            }
        };
        let on_enter = typed.clone();
        entry.connect_activate(move |_| on_enter());
        send.connect_clicked(move |_| typed());
        card.add_controller(keys);
        let focus = if question.choices.is_empty() {
            entry.upcast::<gtk::Widget>()
        } else {
            card.clone().upcast()
        };
        focus.set_focusable(true);
        glib::idle_add_local_once(move || {
            focus.grab_focus();
        });
        card
    }

    /// A request card's frame and header: its kind, what it is about, and its age.
    fn card(&self, kind: &str, about: &str, since: Timestamp, more: usize) -> gtk::Box {
        let card = vbox(12);
        card.add_css_class("request");
        let header = hbox(8);
        header.append(
            &gtk::Label::builder()
                .label(kind)
                .css_classes(["request-kind"])
                .build(),
        );
        if !about.is_empty() {
            header.append(
                &gtk::Label::builder()
                    .label("·")
                    .css_classes(["muted"])
                    .build(),
            );
            header.append(
                &gtk::Label::builder()
                    .label(about)
                    .css_classes(["heading"])
                    .build(),
            );
        }
        let age = gtk::Label::builder()
            .label(age(since))
            .hexpand(true)
            .xalign(1.0)
            .css_classes(["muted", "caption", "request-age"])
            .build();
        self.state.borrow_mut().ages.push((age.clone(), since));
        header.append(&age);
        if more > 0 {
            header.append(
                &gtk::Label::builder()
                    .label(format!("+{more} more"))
                    .css_classes(["muted", "caption"])
                    .build(),
            );
        }
        card.append(&header);
        card
    }

    /// The header, the composer's controls and the status line, from the state.
    fn redraw_chrome(&self) {
        let state = self.state.borrow();
        let session = &state.session;
        let account = session
            .account_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let mode = mode_name(session.permission_mode);
        self.page.set_title(&session.title());
        self.title.set_title(&session.title());
        let (glyph, word, class) = state_mark(state.status, session);
        self.status.set_label(&format!("{glyph} {word}"));
        self.status.set_css_classes(&[class, "caption-heading"]);
        // Narrow, the header has no room for the state beside the title: the subtitle says
        // it, with the model, which the composer then shows only as an icon.
        let subtitle = if state.compact {
            [format!("{glyph} {word}"), session.model.clone()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        } else {
            [state.machine_name.as_str(), session.repo.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        };
        self.title.set_subtitle(&subtitle);
        self.status.set_visible(!state.compact);

        let c = &self.composer;
        c.account_label.set_label(if account.is_empty() {
            "account"
        } else {
            &account
        });
        c.model_label.set_label(if session.model.is_empty() {
            "default model"
        } else {
            &session.model
        });
        c.model
            .set_tooltip_text(Some(&format!("Model: {}", session.model)));
        c.mode_label.set_label(mode);
        c.mode.set_tooltip_text(Some(&format!(
            "Permission mode: {}",
            mode_description(session.permission_mode)
        )));
        c.account
            .set_tooltip_text(Some(&format!("Account: {account}")));
        // Narrow, the account and the model are icons; the mode's names are short enough.
        for label in [&c.account_label, &c.model_label] {
            label.set_visible(!state.compact);
        }
        for button in [&c.account, &c.model] {
            if let Some(image) = button.child().and_then(|b| b.first_child()) {
                image.set_visible(state.compact);
            }
        }
        let running = session.running();
        c.stop.set_visible(running);
        c.placeholder.set_label(if running {
            "Queue a prompt for after this turn"
        } else {
            "Write a prompt"
        });
        self.fill_pickers(&state);

        // The account's busiest window, as the TUI's status line shows it.
        let usage = state
            .accounts
            .iter()
            .find(|a| Some(&a.account_id) == session.account_id.as_ref())
            .and_then(|a| {
                a.usage
                    .iter()
                    .max_by(|x, y| x.used_percent.total_cmp(&y.used_percent))
                    .map(|w| {
                        format!(
                            "{} {} {:.0}%",
                            a.account_id,
                            window_label(&w.window),
                            w.used_percent
                        )
                    })
            });
        self.usage.set_label(usage.as_deref().unwrap_or(""));
        self.status_line
            .set_visible((running || usage.is_some()) && state.read_only.is_none());
        self.spinner.set_spinning(running);
        self.spinner.set_visible(running);
        self.working.set_visible(running);
    }

    /// Updates what counts time: the turn's duration and the request's age.
    fn tick(&self) {
        let state = self.state.borrow();
        if let Some(started) = state.session.turn_started {
            let seconds = Timestamp::now().as_second() - started.as_second();
            self.working
                .set_label(&format!("working · {}", duration(seconds)));
        }
        for (label, since) in &state.ages {
            label.set_label(&age(*since));
        }
    }

    /// The account, model and mode pickers' rows.
    fn fill_pickers(&self, state: &State) {
        let c = &self.composer;
        let session = &state.session;
        while let Some(child) = c.accounts.first_child() {
            c.accounts.remove(&child);
        }
        let same: Vec<&Account> = state
            .accounts
            .iter()
            .filter(|a| Some(&a.provider) == session.provider.as_ref())
            .collect();
        let other: Vec<&Account> = state
            .accounts
            .iter()
            .filter(|a| Some(&a.provider) != session.provider.as_ref())
            .collect();
        if state.accounts.is_empty() {
            c.accounts.append(
                &gtk::Label::builder()
                    .label("This machine lists no accounts.")
                    .css_classes(["muted"])
                    .margin_top(12)
                    .margin_bottom(12)
                    .margin_start(12)
                    .margin_end(12)
                    .build(),
            );
        }
        for (heading, accounts) in [
            ("Same provider · the conversation continues", same),
            ("Other provider · replays the transcript", other),
        ] {
            if accounts.is_empty() {
                continue;
            }
            c.accounts.append(
                &gtk::Label::builder()
                    .label(heading)
                    .xalign(0.0)
                    .css_classes(["picker-heading"])
                    .build(),
            );
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["navigation-sidebar"])
                .build();
            for account in accounts {
                let current = Some(&account.account_id) == session.account_id.as_ref();
                let row = picker_row(
                    &account.label,
                    &format!("{} · {}", account.account_id, account.provider.as_str()),
                    current,
                );
                if let Some(window) = account
                    .usage
                    .iter()
                    .max_by(|x, y| x.used_percent.total_cmp(&y.used_percent))
                {
                    let usage = gtk::Label::builder()
                        .label(format!(
                            "{} {:.0}%",
                            window_label(&window.window),
                            window.used_percent
                        ))
                        .css_classes(["usage"])
                        .build();
                    row.insert_child_after(&usage, row.first_child().as_ref());
                }
                list.append(&row);
            }
            let view = self.clone();
            let accounts: Vec<Account> = state
                .accounts
                .iter()
                .filter(|a| {
                    (Some(&a.provider) == session.provider.as_ref()) == heading.starts_with("Same")
                })
                .cloned()
                .collect();
            list.connect_row_activated(move |_, row| {
                let Some(account) = usize::try_from(row.index())
                    .ok()
                    .and_then(|at| accounts.get(at))
                else {
                    return;
                };
                view.switch_account(account);
            });
            c.accounts.append(&list);
        }

        if !c.model_entry.has_focus() {
            c.model_entry.set_text(&session.model);
        }
        while let Some(row) = c.models.first_child() {
            c.models.remove(&row);
        }
        for model in &state.recent {
            c.models
                .append(&picker_row(model, "", *model == session.model));
        }
        c.models.set_visible(!state.recent.is_empty());

        let mut row = c.modes.first_child();
        let mut at = 0;
        while let Some(widget) = row {
            if let Some(check) = widget.first_child().and_then(|b| b.last_child()) {
                check.set_opacity(if MODES.get(at) == Some(&session.permission_mode) {
                    1.0
                } else {
                    0.0
                });
            }
            at += 1;
            row = widget.next_sibling();
        }
    }

    fn switch_account(&self, account: &Account) {
        self.composer.account.popdown();
        let (key, same, current) = {
            let state = self.state.borrow();
            (
                state.key.clone(),
                state.session.provider.as_ref() == Some(&account.provider),
                state.session.account_id.as_ref() == Some(&account.account_id),
            )
        };
        let Some(key) = key else { return };
        if current {
            return;
        }
        self.send(if same {
            CommandBody::SwitchAccount {
                session_id: key.session_id,
                account_id: account.account_id.clone(),
            }
        } else {
            CommandBody::SwitchProvider {
                session_id: key.session_id,
                account_id: account.account_id.clone(),
                model: None,
            }
        });
    }

    fn set_model(&self, model: &str) {
        self.composer.model.popdown();
        let model = model.trim();
        let Some(key) = self.key() else { return };
        if model.is_empty() || model == self.state.borrow().session.model {
            return;
        }
        self.send(CommandBody::SetModel {
            session_id: key.session_id,
            model: model.to_owned(),
        });
    }

    fn set_mode(&self, mode: PermissionMode) {
        self.composer.mode.popdown();
        let Some(key) = self.key() else { return };
        if mode == self.state.borrow().session.permission_mode {
            return;
        }
        self.send(CommandBody::SetPermissionMode {
            session_id: key.session_id,
            mode,
        });
    }

    /// Sends the composer's text as a prompt.
    fn send_prompt(&self) {
        let buffer = self.composer.text.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Some(key) = self.key() else { return };
        if self.state.borrow().read_only.is_some() {
            return;
        }
        self.state.borrow_mut().pending.push(text.to_owned());
        self.send(CommandBody::SendPrompt {
            session_id: key.session_id,
            text: text.to_owned(),
        });
        buffer.set_text("");
        self.follow.set(true);
        self.redraw_streaming();
    }

    fn interrupt(&self) {
        if let Some(key) = self.key() {
            self.send(CommandBody::Interrupt {
                session_id: key.session_id,
            });
        }
    }

    /// Sends `command` to the session's machine; a refusal shows as a toast.
    fn send(&self, command: CommandBody) {
        let (sender, key) = {
            let state = self.state.borrow();
            (state.sender.clone(), state.key.clone())
        };
        let (Some(sender), Some(key)) = (sender, key) else {
            return;
        };
        let prompt = match &command {
            CommandBody::SendPrompt { text, .. } => Some(text.clone()),
            _ => None,
        };
        let reply = sender(key.host_id, command);
        let view = self.clone();
        glib::spawn_future_local(async move {
            if let Err(message) = reply.await {
                if let Some(prompt) = prompt {
                    view.state.borrow_mut().pending.retain(|p| *p != prompt);
                    view.redraw_streaming();
                }
                view.toasts.add_toast(
                    adw::Toast::builder()
                        .title(glib::markup_escape_text(&message))
                        .timeout(10)
                        .build(),
                );
            }
        });
    }

    fn wire(&self) {
        let c = &self.composer;
        // Enter sends, Shift+Enter (or Ctrl+J) adds a line.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let view = self.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let enter = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter);
            if enter
                && !modifiers
                    .intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK)
            {
                view.send_prompt();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        c.text.add_controller(keys);
        let placeholder = c.placeholder.clone();
        c.text.buffer().connect_changed(move |buffer| {
            placeholder.set_visible(buffer.char_count() == 0);
        });
        let view = self.clone();
        c.send.connect_clicked(move |_| view.send_prompt());
        let view = self.clone();
        c.stop.connect_clicked(move |_| view.interrupt());

        let view = self.clone();
        c.model_entry
            .connect_activate(move |entry| view.set_model(&entry.text()));
        let view = self.clone();
        c.models.connect_row_activated(move |_, row| {
            let model = view
                .state
                .borrow()
                .recent
                .get(usize::try_from(row.index()).unwrap_or(usize::MAX))
                .cloned();
            if let Some(model) = model {
                view.set_model(&model);
            }
        });
        let view = self.clone();
        c.modes.connect_row_activated(move |_, row| {
            if let Some(mode) = usize::try_from(row.index())
                .ok()
                .and_then(|at| MODES.get(at))
            {
                view.set_mode(*mode);
            }
        });

        // Follow new items while scrolled to the bottom. Only the user's scrolling decides
        // that: a value change that comes with a new size is the layout's.
        let adjustment = self.scroller.vadjustment();
        let size = Rc::new(Cell::new((0.0, 0.0)));
        let follow = Rc::clone(&self.follow);
        let known = Rc::clone(&size);
        adjustment.connect_value_changed(move |adj| {
            if known.get() == (adj.upper(), adj.page_size()) {
                follow.set(adj.value() + adj.page_size() >= adj.upper() - FOLLOW);
            }
        });
        let resized = {
            let follow = Rc::clone(&self.follow);
            move |adj: &gtk::Adjustment| {
                size.set((adj.upper(), adj.page_size()));
                if follow.get() {
                    adj.set_value(adj.upper() - adj.page_size());
                }
            }
        };
        let on_upper = resized.clone();
        adjustment.connect_upper_notify(move |adj| on_upper(adj));
        adjustment.connect_page_size_notify(move |adj| resized(adj));

        let view = self.clone();
        glib::timeout_add_seconds_local(1, move || {
            if view.page.is_mapped() {
                view.tick();
            }
            glib::ControlFlow::Continue
        });
    }

    #[cfg(test)]
    pub fn scroll_to_end(&self) {
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
    }

    #[cfg(test)]
    pub fn open_picker(&self, name: &str) -> gtk::Popover {
        let button = match name {
            "account" => &self.composer.account,
            "model" => &self.composer.model,
            _ => &self.composer.mode,
        };
        button.popup();
        button.popover().expect("a picker")
    }

    /// Types `text` into the composer and presses Enter.
    #[cfg(test)]
    pub fn submit(&self, text: &str) {
        self.composer.text.buffer().set_text(text);
        self.send_prompt();
    }

    #[cfg(test)]
    pub fn bottom_child(&self) -> Option<String> {
        self.bottom.visible_child_name().map(Into::into)
    }

    #[cfg(test)]
    pub fn transcript_texts(&self) -> Vec<String> {
        let mut texts = transcript::texts(self.transcript.upcast_ref());
        texts.extend(transcript::texts(self.streaming.upcast_ref()));
        texts
    }

    #[cfg(test)]
    pub fn request_texts(&self) -> Vec<String> {
        transcript::texts(self.request.upcast_ref())
    }

    /// Clicks the request card's button labelled `label`.
    #[cfg(test)]
    pub fn click(&self, label: &str) {
        fn find(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(button) = widget.downcast_ref::<gtk::Button>()
                    && button.label().as_deref() == Some(label)
                {
                    return Some(button.clone());
                }
                if let Some(found) = find(&widget, label) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        let root: &gtk::Widget = self.page.upcast_ref();
        find(root, label)
            .expect("the button is shown")
            .emit_clicked();
    }

    /// Picks the question card's choice `at`, as a click on its row.
    #[cfg(test)]
    pub fn answer_choice(&self, at: i32) {
        let list = find::<gtk::ListBox>(self.request.upcast_ref()).expect("the choices");
        let row = list.row_at_index(at).expect("the choice");
        list.emit_by_name::<()>("row-activated", &[&row]);
    }

    /// Types an answer in the question card and presses Enter.
    #[cfg(test)]
    pub fn answer_text(&self, text: &str) {
        let entry = find::<gtk::Entry>(self.request.upcast_ref()).expect("the answer entry");
        entry.set_text(text);
        entry.emit_activate();
    }

    #[cfg(test)]
    pub fn controls(&self) -> (String, String, String) {
        let c = &self.composer;
        (
            c.account_label.label().into(),
            c.model_label.label().into(),
            c.mode_label.label().into(),
        )
    }

    #[cfg(test)]
    pub fn pick_account(&self, at: usize) {
        let accounts = self.state.borrow().accounts.clone();
        let mut ordered: Vec<&Account> = Vec::new();
        let provider = self.state.borrow().session.provider.clone();
        ordered.extend(
            accounts
                .iter()
                .filter(|a| Some(&a.provider) == provider.as_ref()),
        );
        ordered.extend(
            accounts
                .iter()
                .filter(|a| Some(&a.provider) != provider.as_ref()),
        );
        self.switch_account(ordered[at]);
    }

    #[cfg(test)]
    pub fn pick_model(&self, model: &str) {
        self.composer.model_entry.set_text(model);
        self.composer.model_entry.emit_activate();
    }

    #[cfg(test)]
    pub fn pick_mode(&self, mode: PermissionMode) {
        self.set_mode(mode);
    }
}

impl Composer {
    fn new() -> Self {
        let text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(8)
            .right_margin(8)
            .build();
        let placeholder = gtk::Label::builder()
            .xalign(0.0)
            .yalign(0.0)
            .can_target(false)
            .css_classes(["placeholder"])
            .build();
        let (account, account_label) = control("avatar-default-symbolic");
        let (model, model_label) = control("system-run-symbolic");
        let (mode, mode_label) = control("");
        model_label.set_ellipsize(pango::EllipsizeMode::Middle);
        model_label.set_max_width_chars(24);
        let send = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Send (Enter)")
            .css_classes(["circular", "suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        let stop = gtk::Button::builder()
            .icon_name("media-playback-stop-symbolic")
            .tooltip_text("Stop the turn")
            .css_classes(["circular"])
            .valign(gtk::Align::Center)
            .build();

        let accounts = vbox(0);
        accounts.add_css_class("picker");
        account.set_popover(Some(&gtk::Popover::builder().child(&accounts).build()));

        let model_entry = gtk::Entry::builder()
            .placeholder_text("Model, in the provider's naming")
            .margin_start(12)
            .margin_end(12)
            .build();
        let models = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        let model_box = vbox(6);
        model_box.add_css_class("picker");
        model_box.append(
            &gtk::Label::builder()
                .label("Model · switches between turns")
                .xalign(0.0)
                .css_classes(["picker-heading"])
                .build(),
        );
        model_box.append(&model_entry);
        model_box.append(
            &gtk::Label::builder()
                .label("Recent")
                .xalign(0.0)
                .css_classes(["picker-heading"])
                .build(),
        );
        model_box.append(&models);
        let recent_heading = model_box.last_child().and_then(|w| w.prev_sibling());
        if let Some(heading) = recent_heading {
            models
                .bind_property("visible", &heading, "visible")
                .sync_create()
                .build();
        }
        model.set_popover(Some(&gtk::Popover::builder().child(&model_box).build()));

        let modes = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        for mode_value in MODES {
            modes.append(&picker_row(
                mode_name(mode_value),
                mode_description(mode_value),
                false,
            ));
        }
        let mode_box = vbox(0);
        mode_box.add_css_class("picker");
        mode_box.append(
            &gtk::Label::builder()
                .label("Permission mode")
                .xalign(0.0)
                .css_classes(["picker-heading"])
                .build(),
        );
        mode_box.append(&modes);
        mode.set_popover(Some(&gtk::Popover::builder().child(&mode_box).build()));

        Self {
            text,
            placeholder,
            send,
            stop,
            account,
            account_label,
            accounts,
            model,
            model_label,
            model_entry,
            models,
            mode,
            mode_label,
            modes,
        }
    }

    /// The composer's frame: the editor above its controls.
    fn frame(&self) -> gtk::Box {
        let frame = vbox(4);
        frame.add_css_class("composer");
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            // A scrollbar's own minimum would make an empty editor two lines tall: it
            // shows only once the prompt outgrows the editor.
            .vscrollbar_policy(gtk::PolicyType::External)
            .propagate_natural_height(true)
            .max_content_height(EDITOR_HEIGHT)
            .child(&self.text)
            .build();
        let grown = scroller.clone();
        let text = self.text.clone();
        self.text.buffer().connect_changed(move |_| {
            let (_, natural, _, _) = text.measure(gtk::Orientation::Vertical, text.width());
            grown.set_vscrollbar_policy(if natural > EDITOR_HEIGHT {
                gtk::PolicyType::Automatic
            } else {
                gtk::PolicyType::External
            });
        });
        let overlay = gtk::Overlay::builder().child(&scroller).build();
        overlay.add_overlay(&self.placeholder);
        frame.append(&overlay);
        let controls = hbox(2);
        controls.append(&self.account);
        controls.append(&self.model);
        controls.append(&self.mode);
        let spacer = gtk::Box::builder().hexpand(true).build();
        controls.append(&spacer);
        controls.append(&self.stop);
        controls.append(&self.send);
        let gap = gtk::Box::builder().width_request(4).build();
        controls.append(&gap);
        frame.append(&controls);
        frame
    }
}

/// The first widget of type `W` under `widget`.
#[cfg(test)]
fn find<W: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<W> {
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Ok(found) = widget.clone().downcast::<W>() {
            return Some(found);
        }
        if let Some(found) = find(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

/// A composer control: a flat menu button with a label, and an icon shown in its place when
/// narrow; none for an empty `icon`.
fn control(icon: &str) -> (gtk::MenuButton, gtk::Label) {
    let label = gtk::Label::new(None);
    let content = hbox(6);
    if !icon.is_empty() {
        let image = gtk::Image::from_icon_name(icon);
        image.set_visible(false);
        content.append(&image);
    }
    content.append(&label);
    let button = gtk::MenuButton::builder()
        .child(&content)
        .always_show_arrow(true)
        .css_classes(["flat", "control"])
        .build();
    (button, label)
}

/// A picker row: title, an optional subtitle, and a check when it is the current choice.
fn picker_row(title: &str, subtitle: &str, current: bool) -> gtk::Box {
    let row = hbox(12);
    let text = vbox(2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    text.append(&gtk::Label::builder().label(title).xalign(0.0).build());
    if !subtitle.is_empty() {
        text.append(
            &gtk::Label::builder()
                .label(subtitle)
                .xalign(0.0)
                .wrap(true)
                .max_width_chars(36)
                .css_classes(["muted", "caption"])
                .build(),
        );
    }
    row.append(&text);
    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.set_valign(gtk::Align::Center);
    check.set_opacity(if current { 1.0 } else { 0.0 });
    row.append(&check);
    row
}

/// A request's command or diff, monospace, scrolling past [`REQUEST_LINES`].
fn command_view(command: &str) -> gtk::Widget {
    let label = wrapped(command, true);
    label.add_css_class("mono");
    let lines = i32::try_from(command.lines().count()).unwrap_or(i32::MAX);
    if lines <= REQUEST_LINES {
        label.add_css_class("command");
        return label.upcast();
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(REQUEST_LINES * 18)
        .child(&label)
        .css_classes(["command"])
        .build();
    scroller.upcast()
}

/// Why a request is the user's, or that it went to the primary session first.
fn why(
    card: &gtk::Box,
    routed_to: Route,
    reason: Option<herder_protocol::EscalationReason>,
    note: Option<&str>,
) {
    let mut lines = Vec::new();
    if routed_to == Route::Primary {
        lines.push("Asked the primary session first; you can answer too.".to_owned());
    } else if let Some(reason) = reason {
        lines.push(format!("Escalated: {}.", reason_text(reason)));
    }
    if let Some(note) = note {
        lines.push(format!("The primary says: {}", first_line(note)));
    }
    for line in lines {
        let label = wrapped(&line, false);
        label.add_css_class("muted");
        card.append(&label);
    }
}

fn shortcut(key: gdk::Key, action: impl Fn() + 'static) -> gtk::Shortcut {
    gtk::Shortcut::new(
        Some(gtk::KeyvalTrigger::new(key, gdk::ModifierType::empty())),
        Some(gtk::CallbackAction::new(move |_, _| {
            action();
            glib::Propagation::Stop
        })),
    )
}

fn digit(n: usize) -> Option<gdk::Key> {
    Some(match n {
        1 => gdk::Key::_1,
        2 => gdk::Key::_2,
        3 => gdk::Key::_3,
        4 => gdk::Key::_4,
        5 => gdk::Key::_5,
        6 => gdk::Key::_6,
        7 => gdk::Key::_7,
        8 => gdk::Key::_8,
        9 => gdk::Key::_9,
        _ => return None,
    })
}

/// How long ago `since` was, as a request's header says it.
fn age(since: Timestamp) -> String {
    format!(
        "asked {} ago",
        duration(Timestamp::now().as_second() - since.as_second())
    )
}

/// A limit window's short name, as the TUI's.
fn window_label(window: &str) -> String {
    match window {
        "five_hour" => "5h".to_owned(),
        "seven_day" | "weekly" => "week".to_owned(),
        "daily" => "day".to_owned(),
        other => other.replace('_', " "),
    }
}

/// A session's state as docs/tui-design.md §3.2 marks it: glyph, words and colour.
pub fn state_mark(
    status: SessionStatus,
    session: &Session,
) -> (&'static str, &'static str, &'static str) {
    let waiting_on_you = session.approvals.iter().any(|a| a.routed_to == Route::User)
        || session.questions.iter().any(|q| q.routed_to == Route::User);
    match status {
        SessionStatus::NeedsYou => ("◉", "needs you", "state-attention"),
        _ if waiting_on_you && status == SessionStatus::Running => {
            ("◉", "needs you", "state-attention")
        }
        SessionStatus::Error => ("✗", "error", "state-error"),
        SessionStatus::Running => ("●", "running", "state-running"),
        SessionStatus::WaitingForCapacity => ("◌", "waiting", "state-waiting"),
        SessionStatus::Idle => ("○", "idle", "state-idle"),
        SessionStatus::Archived => ("▪", "archived", "state-idle"),
        SessionStatus::Moved => ("→", "moved", "state-waiting"),
        SessionStatus::Unknown => ("·", "unknown", "state-idle"),
    }
}
