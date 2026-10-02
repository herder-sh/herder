//! Shared client core used by the TUI and the native apps.

/// Name of this crate, used to prove the stub compiles and links.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_matches_package() {
        assert_eq!(super::CRATE_NAME, "herder-client-core");
    }
}
