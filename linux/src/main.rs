//! The GTK4 desktop app: paired machines and their sessions, over
//! [`herder_client_core::Client`] like the TUI.
//!
//! - `app`: the tokio runtime the client runs on, the application, and the wiring between
//!   the client and the window.
//! - `lists`: the session lists, built from plain [`herder_client_core::Machine`] values and
//!   what each session's subscription said.
//! - `session`: one session's transcript and state, folded from its subscription.
//! - `window`: the main window, as the Mac app's: the sidebar, the lists and the session.
//! - `machines`: the machines page, and adding a machine.
//! - `new_session`: the New Session dialog.
//! - `prs`: pull requests, in the session view and in the window's list of all of them.
//! - `session_view`, `transcript`, `tools`, `markdown`: the open session, its transcript and
//!   composer.
//! - `images`: images attached to prompts, their thumbnails and the full-size viewer.
//! - `theme`: the colour tokens and the rules that use them.
//!
//! GTK and libadwaita are Linux's, so on any other OS the binary only says where it runs and
//! the workspace still builds there.

#[cfg(target_os = "linux")]
mod app;
#[cfg(all(test, target_os = "linux"))]
mod e2e;
#[cfg(target_os = "linux")]
mod images;
#[cfg(target_os = "linux")]
mod lists;
#[cfg(target_os = "linux")]
mod machines;
#[cfg(target_os = "linux")]
mod markdown;
#[cfg(target_os = "linux")]
mod new_session;
#[cfg(target_os = "linux")]
mod prs;
#[cfg(all(test, target_os = "linux"))]
mod screenshots;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod session_view;
#[cfg(target_os = "linux")]
mod theme;
#[cfg(target_os = "linux")]
mod tools;
#[cfg(target_os = "linux")]
mod transcript;
#[cfg(all(test, target_os = "linux"))]
mod view_tests;
#[cfg(target_os = "linux")]
mod window;

#[cfg(target_os = "linux")]
fn main() -> gtk::glib::ExitCode {
    app::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("herder's desktop app runs on Linux; on a Mac, use the herder app for macOS");
    std::process::exit(1);
}
