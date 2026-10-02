//! The ratatui terminal client.

/// Runs the TUI. Not implemented yet; returns the message to show the user.
pub fn run() -> &'static str {
    "herder tui: not implemented yet"
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_reports_not_implemented() {
        assert!(super::run().contains("not implemented yet"));
    }
}
