//! Daemon runtime: the WebSocket server and the agent sessions it hosts.

/// Runs the daemon. Not implemented yet; returns the message to show the user.
pub fn run() -> &'static str {
    "herder daemon: not implemented yet"
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_reports_not_implemented() {
        assert!(super::run().contains("not implemented yet"));
    }
}
