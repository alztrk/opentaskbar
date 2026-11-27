//! Wi-Fi flyout data model and UI state (C# `FjordBarWindow.Wifi.cs` +
//! `Services/Interop/WifiNetworks.cs`).
//!
//! The backend is `crate::platform::wifi::{scan_networks, connect, disconnect}`.
//! [`NetworkView::from_platform`] is the only place that maps its `WifiNetwork`
//! (SSID, signal, security, saved-profile flag and real BSS availability) onto
//! the C#-style view model fields (`authentication`, `cipher`, `has_profile`, ...).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// @pure-start
/// Signal buckets used by the C# strength text/percentage display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalClass {
    None,
    Weak,
    Fair,
    Good,
    Excellent,
}

impl SignalClass {
    pub fn from_signal(signal: u32) -> Self {
        match signal {
            0..=19 => Self::None,
            20..=39 => Self::Weak,
            40..=59 => Self::Fair,
            60..=79 => Self::Good,
            _ => Self::Excellent,
        }
    }

    /// Segoe Fluent Icons glyph for the strength indicator.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::None => "\u{e701}",
            Self::Weak => "\u{e702}",
            Self::Fair => "\u{e703}",
            Self::Good => "\u{e704}",
            Self::Excellent => "\u{e705}",
        }
    }

    pub fn percent_text(self) -> String {
        let value = match self {
            Self::None => 0,
            Self::Weak => 25,
            Self::Fair => 50,
            Self::Good => 75,
            Self::Excellent => 100,
        };
        format!("{value}%")
    }
}

/// A network reduced to what the flyout renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkView {
    pub name: String,
    pub ssid: String,
    pub signal: u32,
    pub secured: bool,
    pub connected: bool,
    pub connectable: bool,
    pub has_profile: bool,
    pub authentication: u32,
    pub cipher: u32,
}

impl NetworkView {
    pub fn signal_class(&self) -> SignalClass {
        SignalClass::from_signal(self.signal)
    }

    /// `WifiNetworks.Security` mapping to a locale key.
    pub fn security_key(&self) -> &'static str {
        security_key(self.authentication, self.cipher, self.secured)
    }

    /// `WifiNetworks.Usable` (a known profile or a supported temporary profile).
    pub fn usable(&self) -> bool {
        self.connectable
            && (self.has_profile
                || supports_temporary_profile(self.authentication, self.cipher, self.secured))
    }

    pub fn needs_password(&self) -> bool {
        self.secured && !self.has_profile && self.usable()
    }

    pub fn identity(&self) -> (String, bool, u32, u32) {
        (
            self.ssid.clone(),
            self.secured,
            self.authentication,
            self.cipher,
        )
    }
}

pub fn supports_temporary_profile(authentication: u32, cipher: u32, secured: bool) -> bool {
    match (authentication, cipher) {
        (1, 0) => !secured,
        (4 | 7, 2 | 4) | (9, 4) => true,
        _ => false,
    }
}

pub fn security_key(authentication: u32, cipher: u32, secured: bool) -> &'static str {
    match (authentication, cipher) {
        (1, 0) if !secured => "OpenNetwork",
        (4, 2 | 4) => "WpaPersonal",
        (7, 2 | 4) => "Wpa2Personal",
        (9, 4) => "Wpa3Personal",
        _ => "SecuredNetwork",
    }
}

/// `ReadAsync` ordering: connected, usable, signal, then culture-insensitive name.
pub fn sort_networks(networks: &mut [NetworkView]) {
    networks.sort_by(|left, right| {
        right
            .connected
            .cmp(&left.connected)
            .then_with(|| right.usable().cmp(&left.usable()))
            .then_with(|| right.signal.cmp(&left.signal))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
}

/// Distinct by (SSID, security, authentication, profile) like `DistinctBy`.
pub fn deduplicate(networks: Vec<NetworkView>) -> Vec<NetworkView> {
    let mut seen = std::collections::HashSet::new();
    networks
        .into_iter()
        .filter(|network| seen.insert(network.identity()))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionOutcome {
    Connected,
    Failed,
    Error(u32),
}

impl ConnectionOutcome {
    /// Locale key shown in the flyout status line.
    pub fn status_key(self) -> &'static str {
        match self {
            Self::Connected => "ConnectionEstablished",
            Self::Failed => "WifiConnectFailed",
            Self::Error(_) => "WindowsError",
        }
    }
}

/// `WifiError(Win32Exception)` mapping from `FjordBarWindow.Wifi.cs`.
pub fn error_key(code: u32) -> &'static str {
    match code {
        5 => "LocationDenied",
        1062 => "WlanStopped",
        0x8034_2002 => "WifiOff",
        _ => "WindowsError",
    }
}

#[derive(Clone, Debug, Default)]
pub struct WifiUiState {
    pub networks: Vec<NetworkView>,
    pub selected: Option<usize>,
    pub password: String,
    pub remember: bool,
    pub status: Option<&'static str>,
}

impl WifiUiState {
    pub fn selected_network(&self) -> Option<&NetworkView> {
        self.selected.and_then(|index| self.networks.get(index))
    }

    pub fn select(&mut self, index: Option<usize>) {
        // `SelectionChanged` clears the password on every selection, including re-select.
        self.password.clear();
        self.remember = false;
        self.selected = index.filter(|index| *index < self.networks.len());
    }

    pub fn apply_scan(&mut self, mut networks: Vec<NetworkView>) {
        sort_networks(&mut networks);
        let networks = deduplicate(networks);
        let previous = self.selected_network().map(NetworkView::identity);
        self.networks = networks;
        self.selected = self.networks.iter().position(|network| {
            previous
                .as_ref()
                .is_some_and(|identity| network.identity() == *identity)
        });
        self.status = if self.networks.is_empty() {
            Some("WifiNoNetworks")
        } else {
            None
        };
    }

    pub fn password_required(&self) -> bool {
        self.selected_network()
            .is_some_and(NetworkView::needs_password)
    }

    pub fn action_key(&self) -> &'static str {
        if self
            .selected_network()
            .is_some_and(|network| network.connected)
        {
            "WifiDisconnect"
        } else {
            "WifiConnect"
        }
    }

    pub fn can_connect(&self) -> bool {
        self.selected_network()
            .is_some_and(|network| network.connected || network.usable())
    }
}

/// Cancellable wait for the 750 ms poll loop and Wi-Fi scan cool-down.
pub fn yield_tick(duration: Duration, cancel: &AtomicBool) -> bool {
    let deadline = Instant::now() + duration;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}
// @pure-end

/// WS-4 bridge: converts a backend network into the flyout's view model.
pub fn scan_networks() -> Vec<NetworkView> {
    crate::platform::wifi::scan_networks()
        .into_iter()
        .map(NetworkView::from_platform)
        .collect()
}

/// Bridge: connect to an SSID with an optional passphrase. `remember` is
/// forwarded so the platform only persists a WLAN profile when requested.
pub fn connect(ssid: &str, password: &str, remember: bool) -> bool {
    crate::platform::wifi::connect(ssid, password, remember)
}

/// WS-4 bridge: disconnect from the current network.
pub fn disconnect() {
    crate::platform::wifi::disconnect();
}

impl NetworkView {
    /// Field names mirror the C# `WifiNetworks.Network` record. Security codes
    /// come from the real `WifiSecurity`, and profile / connectable come from the
    /// scan (saved WLAN profile + BSS availability), never from constants.
    pub fn from_platform(network: crate::platform::wifi::WifiNetwork) -> Self {
        use crate::platform::wifi::WifiSecurity;

        let secured = network.security != WifiSecurity::Open;
        let authentication = network.security.authentication_code();
        let cipher = network.security.cipher_code();
        Self {
            name: network.ssid.clone(),
            ssid: network.ssid,
            signal: u32::from(network.signal_percent),
            secured,
            connected: network.is_connected,
            connectable: network.connectable,
            has_profile: network.has_profile,
            authentication,
            cipher,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn network(name: &str, signal: u32, secured: bool, connected: bool) -> NetworkView {
        NetworkView {
            name: name.to_owned(),
            ssid: hex::encode(name),
            signal,
            secured,
            connected,
            connectable: true,
            has_profile: false,
            authentication: if secured { 7 } else { 1 },
            cipher: if secured { 4 } else { 0 },
        }
    }

    // Tiny hex helper keeps tests independent of any crate feature.
    mod hex {
        pub fn encode(value: &str) -> String {
            value
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect()
        }
    }

    #[test]
    fn signal_classes_cover_all_buckets() {
        assert_eq!(SignalClass::from_signal(0), SignalClass::None);
        assert_eq!(SignalClass::from_signal(25), SignalClass::Weak);
        assert_eq!(SignalClass::from_signal(50), SignalClass::Fair);
        assert_eq!(SignalClass::from_signal(79), SignalClass::Good);
        assert_eq!(SignalClass::from_signal(100), SignalClass::Excellent);
        assert_eq!(SignalClass::Excellent.percent_text(), "100%");
        assert_ne!(SignalClass::Weak.glyph(), SignalClass::Good.glyph());
    }

    #[test]
    fn security_labels_follow_authentication_and_cipher() {
        assert_eq!(security_key(1, 0, false), "OpenNetwork");
        assert_eq!(security_key(4, 2, true), "WpaPersonal");
        assert_eq!(security_key(7, 4, true), "Wpa2Personal");
        assert_eq!(security_key(9, 4, true), "Wpa3Personal");
        assert_eq!(security_key(3, 99, true), "SecuredNetwork");
    }

    #[test]
    fn temporary_profiles_and_password_requirement() {
        let open = network("Guest", 90, false, false);
        assert!(open.usable());
        assert!(!open.needs_password());
        let mut wpa = network("Home", 80, true, false);
        assert!(wpa.usable());
        assert!(wpa.needs_password());
        wpa.has_profile = true;
        assert!(!wpa.needs_password());
        wpa.has_profile = false;
        wpa.authentication = 3;
        assert!(!wpa.usable());
    }

    #[test]
    fn sort_puts_connected_and_strong_first() {
        let mut networks = vec![
            network("Zeta", 90, false, false),
            network("Weak", 20, false, false),
            network("Home", 70, true, true),
        ];
        sort_networks(&mut networks);
        assert_eq!(networks[0].name, "Home");
        assert_eq!(networks[1].name, "Zeta");
        assert_eq!(networks[2].name, "Weak");
    }

    #[test]
    fn duplicate_ssids_are_removed() {
        let networks = vec![
            network("Home", 70, true, false),
            network("Home", 60, true, false),
        ];
        assert_eq!(deduplicate(networks).len(), 1);
    }

    #[test]
    fn win32_errors_map_to_locale_keys() {
        assert_eq!(error_key(5), "LocationDenied");
        assert_eq!(error_key(1062), "WlanStopped");
        assert_eq!(error_key(0x8034_2002), "WifiOff");
        assert_eq!(error_key(87), "WindowsError");
        assert_eq!(
            ConnectionOutcome::Connected.status_key(),
            "ConnectionEstablished"
        );
        assert_eq!(ConnectionOutcome::Failed.status_key(), "WifiConnectFailed");
    }

    #[test]
    fn ui_state_tracks_selection_and_password() {
        let mut state = WifiUiState::default();
        state.apply_scan(vec![network("Home", 70, true, false)]);
        assert_eq!(state.status, None);
        state.select(Some(0));
        assert!(state.password_required());
        assert_eq!(state.action_key(), "WifiConnect");
        assert!(state.can_connect());
        state.password.push_str("secret");
        state.select(Some(0));
        assert!(state.password.is_empty());
    }

    #[test]
    fn empty_scan_reports_no_networks() {
        let mut state = WifiUiState::default();
        state.apply_scan(Vec::new());
        assert_eq!(state.status, Some("WifiNoNetworks"));
        assert!(state.selected_network().is_none());
        assert!(!state.can_connect());
    }

    #[test]
    fn platform_security_maps_to_real_authentication_cipher_and_profile() {
        use crate::platform::wifi::{WifiNetwork, WifiSecurity};

        let cases = [
            (WifiSecurity::Open, false, 1, 0, "OpenNetwork"),
            (WifiSecurity::Wpa, true, 4, 2, "WpaPersonal"),
            (WifiSecurity::Wpa2, true, 7, 4, "Wpa2Personal"),
            (WifiSecurity::Wpa3, true, 9, 4, "Wpa3Personal"),
        ];
        for (security, secured, auth, cipher, key) in cases {
            let view = NetworkView::from_platform(
                WifiNetwork::new("Home".into(), 80, security, false)
                    .with_profile(true)
                    .with_connectable(true),
            );
            assert_eq!(view.secured, secured);
            assert_eq!(view.authentication, auth);
            assert_eq!(view.cipher, cipher);
            assert_eq!(view.security_key(), key);
            assert!(view.has_profile);
            assert!(view.connectable);
            assert!(supports_temporary_profile(auth, cipher, secured));
        }
    }

    #[test]
    fn platform_profile_and_availability_drive_usable_and_password() {
        use crate::platform::wifi::{WifiNetwork, WifiSecurity};

        let saved = NetworkView::from_platform(
            WifiNetwork::new("Saved".into(), 80, WifiSecurity::Wpa2, false)
                .with_profile(true)
                .with_connectable(true),
        );
        assert!(saved.usable());
        assert!(!saved.needs_password());

        let new_secured = NetworkView::from_platform(
            WifiNetwork::new("New".into(), 80, WifiSecurity::Wpa3, false).with_connectable(true),
        );
        assert!(!new_secured.has_profile);
        assert!(new_secured.usable());
        assert!(new_secured.needs_password());

        let out_of_range = NetworkView::from_platform(
            WifiNetwork::new("Far".into(), 80, WifiSecurity::Wpa2, false)
                .with_profile(true)
                .with_connectable(false),
        );
        assert!(!out_of_range.usable());
        assert!(!out_of_range.needs_password());
    }

    #[test]
    fn yield_tick_stops_on_cancellation() {
        use std::sync::Arc;
        let immediately = AtomicBool::new(true);
        assert!(!yield_tick(Duration::from_secs(10), &immediately));
        let cancel = Arc::new(AtomicBool::new(false));
        let other = Arc::clone(&cancel);
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            other.store(true, Ordering::SeqCst);
        });
        assert!(!yield_tick(Duration::from_secs(10), &cancel));
        stopper.join().unwrap();
    }
}
