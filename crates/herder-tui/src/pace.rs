//! When the event loop draws: after input at once; after what the machines sent (sessions,
//! host resources) at most every [`BACKGROUND_FRAME`], so a burst of updates draws once; while
//! a spinner turns, every [`SPINNER_FRAME`]; and otherwise never. An idle desktop screen is
//! drawn no more than it changes, and every frame is a diff against the last, so only the
//! cells that changed are written.
//!
//! A phone over mosh may still lose track of the screen. On a narrow screen the whole frame is
//! written again every [`RESYNC`] while idle ([`Paint::Resync`]): every cell over the shown
//! one, never a clear, so nothing blanks. A clear and full repaint ([`Paint::Full`]) happens
//! only on a resize, when the terminal comes back to the front, and on the first frame.

use std::time::{Duration, Instant};

use crate::app::App;
use crate::views::{NARROW, Paint};

/// How often the screen is drawn while a spinner turns: the slower glyph set's frame.
pub const SPINNER_FRAME: Duration = Duration::from_millis(120);

/// The least time between two frames drawn for what the machines sent.
pub const BACKGROUND_FRAME: Duration = Duration::from_millis(250);

/// How long a narrow screen sits idle before its frame is written again.
pub const RESYNC: Duration = Duration::from_secs(5);

/// The loop's drawing schedule.
#[derive(Debug)]
pub struct Pace {
    /// When the last frame was drawn.
    last_paint: Instant,
    /// When every cell was last written, by a full repaint or a resync.
    last_resync: Instant,
    /// What the machines sent waits to be drawn.
    pending: bool,
}

impl Pace {
    pub fn new(now: Instant) -> Self {
        Self {
            last_paint: now,
            last_resync: now,
            pending: false,
        }
    }

    /// Notes a frame drawn `how` at `now`.
    pub fn painted(&mut self, how: Paint, now: Instant) {
        self.last_paint = now;
        self.pending = false;
        if how != Paint::Diff {
            self.last_resync = now;
        }
    }

    /// Whether to draw after a batch of messages: at once for `input`, which the user waits
    /// on; for what the machines sent, once [`BACKGROUND_FRAME`] passed since the last frame,
    /// else later, with what else comes by then.
    pub fn after(&mut self, input: bool, now: Instant) -> Option<Paint> {
        if input || now.duration_since(self.last_paint) >= BACKGROUND_FRAME {
            return Some(Paint::Diff);
        }
        self.pending = true;
        None
    }

    /// How long the loop may wait for a message before it draws on its own; `None` waits
    /// until one comes.
    pub fn wait(&self, app: &App, now: Instant) -> Option<Duration> {
        let since_paint = now.duration_since(self.last_paint);
        let mut due = Vec::new();
        if app.animating() {
            due.push(SPINNER_FRAME.saturating_sub(since_paint));
        }
        if self.pending {
            due.push(BACKGROUND_FRAME.saturating_sub(since_paint));
        }
        if narrow(app) {
            due.push(RESYNC.saturating_sub(now.duration_since(self.last_resync)));
        }
        due.into_iter().min()
    }

    /// What to draw once a [`Pace::wait`] ran out with no message; `None` when nothing
    /// changed.
    pub fn lapsed(&self, app: &App, now: Instant) -> Option<Paint> {
        if narrow(app) && now.duration_since(self.last_resync) >= RESYNC {
            return Some(Paint::Resync);
        }
        let since_paint = now.duration_since(self.last_paint);
        let spinner = app.animating() && since_paint >= SPINNER_FRAME;
        let background = self.pending && since_paint >= BACKGROUND_FRAME;
        (spinner || background).then_some(Paint::Diff)
    }
}

/// Whether the screen is laid out as a phone's, where the terminal may be mosh's.
fn narrow(app: &App) -> bool {
    app.width > 0 && app.width < NARROW
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;

    /// [`fake::tree`] drawn `width` columns wide, nothing running in view.
    fn idle(width: u16) -> App {
        let mut app = fake::tree();
        app.width = width;
        assert!(!app.animating());
        app
    }

    #[test]
    fn an_idle_desktop_screen_is_never_drawn_on_a_timer() {
        let app = idle(119);
        let start = Instant::now();
        let mut pace = Pace::new(start);
        pace.painted(Paint::Full, start);
        // The loop waits for a message, however long none comes.
        assert_eq!(pace.wait(&app, start), None);
        for secs in [1, 5, 60, 3600] {
            assert_eq!(pace.lapsed(&app, start + Duration::from_secs(secs)), None);
        }
    }

    #[test]
    fn a_spinner_draws_its_frames_and_stops_with_the_turn() {
        let mut app = fake::chat();
        app.width = 119;
        assert!(app.animating());
        let start = Instant::now();
        let mut pace = Pace::new(start);
        pace.painted(Paint::Diff, start);
        assert_eq!(pace.wait(&app, start), Some(SPINNER_FRAME));
        assert_eq!(pace.lapsed(&app, start + SPINNER_FRAME), Some(Paint::Diff));
        // Out of view (another tab): no more frames.
        app.focus = crate::app::Focus::Tasks;
        assert!(!app.animating());
        assert_eq!(pace.wait(&app, start), None);
    }

    #[test]
    fn what_the_machines_send_is_drawn_at_most_every_background_frame() {
        let app = idle(119);
        let start = Instant::now();
        let mut pace = Pace::new(start);
        pace.painted(Paint::Diff, start);
        // A resource update right after a frame waits, and the next ones join it.
        let soon = start + Duration::from_millis(40);
        assert_eq!(pace.after(false, soon), None);
        assert_eq!(pace.after(false, soon), None);
        assert_eq!(
            pace.wait(&app, soon),
            Some(BACKGROUND_FRAME - Duration::from_millis(40))
        );
        let due = start + BACKGROUND_FRAME;
        assert_eq!(pace.lapsed(&app, due), Some(Paint::Diff));
        pace.painted(Paint::Diff, due);
        assert_eq!(pace.wait(&app, due), None);
        // Input draws at once.
        assert_eq!(pace.after(true, due), Some(Paint::Diff));
    }

    #[test]
    fn a_narrow_screen_is_rewritten_now_and_then_without_a_clear() {
        let app = idle(44);
        let start = Instant::now();
        let mut pace = Pace::new(start);
        pace.painted(Paint::Full, start);
        assert_eq!(pace.wait(&app, start), Some(RESYNC));
        assert_eq!(pace.lapsed(&app, start + RESYNC / 2), None);
        assert_eq!(pace.lapsed(&app, start + RESYNC), Some(Paint::Resync));
    }
}
