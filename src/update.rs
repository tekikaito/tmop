//! Update check stub.
//!
//! TMOP performs no network access at all. `checkForUpdatesOnStart` is kept only for
//! settings-file compatibility.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Disabled,
}

/// Always returns [`UpdateStatus::Disabled`] without touching the network.
pub fn check_for_updates() -> UpdateStatus {
    UpdateStatus::Disabled
}

#[cfg(test)]
mod tests {
    #[test]
    fn update_check_is_inert() {
        assert_eq!(super::check_for_updates(), super::UpdateStatus::Disabled);
    }
}
