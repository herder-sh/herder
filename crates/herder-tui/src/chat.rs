//! The open session's transcript as the user moves through it: the item cursor `[` / `]`
//! walks, what `e` (or a tap) expanded, and what `/thinking` and `/details` hide. `o` opens a
//! prompt's images with the desktop's viewer and `w` saves them: the prompt's under the
//! cursor, else the latest one's.
//!
//! OpenCode expands a tool call with a click only; the item cursor reaches every foldable
//! thing from the keyboard too.

use std::collections::HashSet;

use herder_protocol::{CommandBody, Item, ItemBody, ItemId};

use crate::app::{App, Effect};
use crate::compose::Origin;
use crate::session::Entry;

/// What the user does to the transcript; see [`crate::action::Action::Chat`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatAct {
    /// Move the item cursor to the previous item.
    Prev,
    /// Move the item cursor to the next item.
    Next,
    /// Expand or collapse the item under the cursor.
    Toggle,
    /// Expand or collapse the `n`th item drawn, as a tap does, and put the cursor on it.
    ToggleAt(usize),
    /// Copy the item under the cursor to the clipboard.
    Copy,
    /// Stop the running turn.
    Stop,
    /// Open the images of the prompt under the cursor, or the latest prompt's.
    OpenImages,
    /// Save those images to files.
    SaveImages,
}

/// The transcript's cursor and folds.
#[derive(Debug)]
pub struct Chat {
    /// The item under the cursor; `None` until `[` or `]`.
    pub cursor: Option<ItemId>,
    /// Items expanded past their one line or first rows.
    pub expanded: HashSet<ItemId>,
    /// Whether reasoning is shown.
    pub thinking: bool,
    /// Whether tool output and diffs are shown.
    pub details: bool,
    /// The items the cursor can reach, in order, as the last frame drew them.
    pub items: Vec<ItemId>,
    /// The cursor moved: the next frame scrolls it into view.
    pub reveal: bool,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            cursor: None,
            expanded: HashSet::new(),
            thinking: true,
            details: true,
            items: Vec::new(),
            reveal: false,
        }
    }
}

impl Chat {
    /// Forgets the cursor, as when another session opens.
    pub fn reset(&mut self) {
        self.cursor = None;
        self.items.clear();
    }

    fn toggle(&mut self, id: ItemId) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
    }
}

impl App {
    /// Carries out one [`ChatAct`].
    pub(crate) fn chat_act(&mut self, act: ChatAct) -> Vec<Effect> {
        let chat = &mut self.chat;
        let at = chat
            .cursor
            .as_ref()
            .and_then(|id| chat.items.iter().position(|item| item == id));
        match act {
            ChatAct::Prev | ChatAct::Next => {
                let Some(last) = chat.items.len().checked_sub(1) else {
                    return Vec::new();
                };
                let next = match (at, act) {
                    (None, _) => last,
                    (Some(at), ChatAct::Prev) => at.saturating_sub(1),
                    (Some(at), _) => (at + 1).min(last),
                };
                chat.cursor = chat.items.get(next).cloned();
                chat.reveal = true;
            }
            ChatAct::Toggle => {
                if let Some(id) = chat.cursor.clone() {
                    chat.toggle(id);
                }
            }
            ChatAct::ToggleAt(at) => {
                if let Some(id) = chat.items.get(at).cloned() {
                    chat.cursor = Some(id.clone());
                    chat.toggle(id);
                }
            }
            ChatAct::Copy => {
                let cursor = chat.cursor.clone();
                let text = cursor.and_then(|id| self.item_text(&id));
                return match text {
                    Some(text) => {
                        self.notice = Some("copied".to_owned());
                        vec![Effect::Copy(text)]
                    }
                    None => Vec::new(),
                };
            }
            ChatAct::OpenImages => return self.fetch_images(true),
            ChatAct::SaveImages => return self.fetch_images(false),
            ChatAct::Stop => {
                if let Some(key) = &self.open
                    && let Some(session) = self.sessions.get(key)
                    && session.turn.is_some()
                {
                    let command = CommandBody::Interrupt {
                        session_id: session.id.clone(),
                    };
                    return vec![Effect::Send {
                        host_id: key.host_id.clone(),
                        command,
                        origin: Origin::Session(key.clone()),
                    }];
                }
            }
        }
        Vec::new()
    }

    /// Fetches the images of the prompt under the cursor, else of the latest prompt that has
    /// any, to `open` or only save once they arrive.
    fn fetch_images(&mut self, open: bool) -> Vec<Effect> {
        let Some((key, session)) = self.open.as_ref().zip(self.open_session()) else {
            return Vec::new();
        };
        // A prompt's files are for its agent, which read them on the machine.
        let images = |item: &Item| match &item.body {
            ItemBody::UserMessage { attachments, .. } => {
                let images: Vec<_> = attachments.iter().filter(|a| a.name.is_none()).collect();
                (!images.is_empty()).then(|| images.into_iter().cloned().collect::<Vec<_>>())
            }
            _ => None,
        };
        let items = || {
            session
                .entries
                .iter()
                .rev()
                .filter_map(|entry| match entry {
                    Entry::Item(item) => Some(item),
                    _ => None,
                })
        };
        let found = match &self.chat.cursor {
            Some(cursor) => items().find(|item| item.id == *cursor).and_then(images),
            None => items().find_map(images),
        };
        let Some(attachments) = found else {
            self.notice = Some("no images here".to_owned());
            return Vec::new();
        };
        let count = attachments.len();
        let effects = attachments
            .into_iter()
            .map(|image| Effect::Send {
                host_id: key.host_id.clone(),
                command: CommandBody::GetAttachment {
                    session_id: key.session_id.clone(),
                    attachment_id: image.attachment_id.clone(),
                },
                origin: Origin::Image {
                    key: key.clone(),
                    attachment_id: image.attachment_id.clone(),
                    name: format!(
                        "herder-{}.{}",
                        image.attachment_id,
                        crate::attach::extension(&image.media_type)
                    ),
                    open,
                },
            })
            .collect();
        let images = if count == 1 { "image" } else { "images" };
        let doing = if open { "opening" } else { "saving" };
        self.notice = Some(format!("{doing} {count} {images}…"));
        effects
    }

    /// The text of the open session's item `id`, as copied: a message's text, or a tool
    /// call's arguments and output.
    fn item_text(&self, id: &ItemId) -> Option<String> {
        let session = self.open_session()?;
        let items = session
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item(item) => Some(item),
                _ => None,
            })
            .chain(&session.streaming);
        let mut found: Option<&Item> = None;
        let mut output = None;
        for item in items {
            if item.id == *id {
                found = Some(item);
            }
            if let ItemBody::ToolResult {
                call_id, output: o, ..
            } = &item.body
                && call_id == id
            {
                output = Some(o.clone());
            }
        }
        match &found?.body {
            ItemBody::UserMessage { text, .. }
            | ItemBody::AssistantMessage { text }
            | ItemBody::Reasoning { text } => Some(text.clone()),
            ItemBody::ToolCall { name, input } => {
                let input = serde_json::to_string_pretty(input).unwrap_or_default();
                Some(match output {
                    Some(output) => format!("{name} {input}\n\n{output}"),
                    None => format!("{name} {input}"),
                })
            }
            ItemBody::ToolResult { output, .. } => Some(output.clone()),
            ItemBody::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::ItemId;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::{Focus, Msg};
    use crate::fake;

    fn draw(app: &mut App, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
        terminal
            .draw(|frame| crate::views::draw(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )))
    }

    #[test]
    fn brackets_walk_the_items_and_e_expands_one() {
        let mut app = fake::chat();
        app.focus = Focus::Transcript;
        let shown = draw(&mut app, 120);
        assert!(!shown.contains("Then a test next to the others."));
        // From the end: the streaming answer, the prompt, the reply...
        press(&mut app, '[');
        assert_eq!(app.chat.cursor, Some(ItemId::new("a2")));
        for _ in 0..8 {
            press(&mut app, '[');
        }
        assert_eq!(app.chat.cursor, Some(ItemId::new("r1")));
        press(&mut app, 'e');
        let shown = draw(&mut app, 120);
        assert!(shown.contains("Then a test next to the others."), "{shown}");
        // Enter on an item expands it too, and copying takes its text.
        app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.chat.expanded.is_empty());
        let effects = press(&mut app, 'c');
        assert!(
            matches!(&effects[..], [Effect::Copy(text)] if text.starts_with("Where the router lives")),
            "{effects:?}"
        );
        // x stops the turn.
        assert!(matches!(
            &press(&mut app, 'x')[..],
            [Effect::Send {
                command: CommandBody::Interrupt { .. },
                ..
            }]
        ));
    }

    #[test]
    fn a_tap_on_a_tool_line_expands_it() {
        let mut app = fake::chat();
        draw(&mut app, 120);
        let at = app
            .chat
            .items
            .iter()
            .position(|id| *id == ItemId::new("c5"))
            .unwrap();
        app.act(crate::action::Action::Chat(ChatAct::ToggleAt(at)));
        assert!(app.chat.expanded.contains(&ItemId::new("c5")));
        assert_eq!(app.chat.cursor, Some(ItemId::new("c5")));
        let shown = draw(&mut app, 120);
        assert!(shown.contains("Write the docs page"), "{shown}");
    }

    #[test]
    fn details_and_thinking_hide_output_and_reasoning() {
        let mut app = fake::chat();
        app.scroll.top = Some(0);
        let shown = draw(&mut app, 120);
        assert!(shown.contains("Thought:"));
        assert!(shown.contains("+.route(\"/health\"") || shown.contains("+ "));
        app.chat.thinking = false;
        app.chat.details = false;
        let shown = draw(&mut app, 120);
        assert!(!shown.contains("Thought:"));
        assert!(!shown.contains("CARGO_PKG_VERSION"));
        assert!(shown.contains("Edit src/api.rs"));
    }
}
