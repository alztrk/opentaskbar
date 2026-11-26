//! C# parity for `Services/Settings/RadiusManager.cs`.
//!
//! Concentric rule: a nested element inset by `d` keeps radius `r - d`. Theme packs supply the
//! reference values so a pack with tighter corners stays tighter while following the user's
//! master dock radius. `DockSheenRadius` rounds only the two top corners.

use super::tokens::CornerRadius;

/// The nine radii C# `RadiusManager.Keys` derives from the dock radius.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NineKeys {
    pub dock_inner_radius: i32,
    pub dock_radius: i32,
    pub control_radius: i32,
    pub popup_radius: i32,
    pub widget_radius: i32,
    pub dock_widget_radius: i32,
    pub dock_widget_action_radius: i32,
    pub widget_field_radius: i32,
    pub widget_quick_action_radius: i32,
}

impl NineKeys {
    pub const KEYS: [&'static str; 9] = [
        "DockInnerRadius",
        "DockRadius",
        "ControlRadius",
        "PopupRadius",
        "WidgetRadius",
        "DockWidgetRadius",
        "DockWidgetActionRadius",
        "WidgetFieldRadius",
        "WidgetQuickActionRadius",
    ];

    /// Design-time radii at the reference dock radius (C# `Radii.xaml` defaults).
    pub const REFERENCES: [(&'static str, i32); 9] = [
        ("DockInnerRadius", 19),
        ("DockRadius", 20),
        ("ControlRadius", 12),
        ("PopupRadius", 12),
        ("WidgetRadius", 12),
        ("DockWidgetRadius", 12),
        ("DockWidgetActionRadius", 5),
        ("WidgetFieldRadius", 7),
        ("WidgetQuickActionRadius", 18),
    ];

    pub fn get(&self, key: &str) -> Option<i32> {
        match key {
            "DockInnerRadius" => Some(self.dock_inner_radius),
            "DockRadius" => Some(self.dock_radius),
            "ControlRadius" => Some(self.control_radius),
            "PopupRadius" => Some(self.popup_radius),
            "WidgetRadius" => Some(self.widget_radius),
            "DockWidgetRadius" => Some(self.dock_widget_radius),
            "DockWidgetActionRadius" => Some(self.dock_widget_action_radius),
            "WidgetFieldRadius" => Some(self.widget_field_radius),
            "WidgetQuickActionRadius" => Some(self.widget_quick_action_radius),
            _ => None,
        }
    }

    pub fn corner(&self, key: &str) -> Option<CornerRadius> {
        self.get(key).map(CornerRadius::uniform)
    }

    pub fn map(&self) -> std::collections::BTreeMap<&'static str, i32> {
        Self::KEYS
            .iter()
            .map(|key| (*key, self.get(key).expect("nine keys")))
            .collect()
    }
}

/// Reference values supplied by a theme pack (key → radius at its own reference dock radius).
pub type RadiusReferences = std::collections::BTreeMap<String, i32>;

/// Derives all nine radii plus the top-only sheen radius.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadiusManager {
    base: i32,
    references: RadiusReferences,
    reference_dock_radius: i32,
}

impl Default for RadiusManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RadiusManager {
    pub const DEFAULT_REFERENCE_DOCK_RADIUS: i32 = 20;
    pub const SHEEN_REFERENCE: i32 = 19;

    pub fn new() -> Self {
        Self {
            base: Self::DEFAULT_REFERENCE_DOCK_RADIUS,
            references: RadiusReferences::new(),
            reference_dock_radius: Self::DEFAULT_REFERENCE_DOCK_RADIUS,
        }
    }

    /// Pure concentric derivation: `max(0, dock - (reference_dock - value))`.
    pub fn derive_value(dock_radius: i32, reference_dock_radius: i32, reference_value: i32) -> i32 {
        (dock_radius - (reference_dock_radius - reference_value)).max(0)
    }

    /// C5 contract: derive all nine keys from a user dock radius.
    pub fn derive(base: i32) -> NineKeys {
        Self::new().derive_for(base)
    }

    /// Uses theme-pack reference values (e.g. a pack whose `DockRadius` is 18).
    pub fn with_references(references: RadiusReferences) -> Self {
        let reference_dock_radius = references
            .get("DockRadius")
            .copied()
            .unwrap_or(Self::DEFAULT_REFERENCE_DOCK_RADIUS);
        Self {
            base: reference_dock_radius,
            references,
            reference_dock_radius,
        }
    }

    pub fn set_base(&mut self, base: i32) {
        self.base = base.clamp(0, 48);
    }

    pub fn base(&self) -> i32 {
        self.base
    }

    pub fn reference_dock_radius(&self) -> i32 {
        self.reference_dock_radius
    }

    fn reference(&self, key: &str, fallback: i32) -> i32 {
        self.references.get(key).copied().unwrap_or(fallback)
    }

    /// Derives with this manager's theme-pack references.
    pub fn derive_for(&self, base: i32) -> NineKeys {
        let r = self.reference_dock_radius;
        let d =
            |key: &str, fallback: i32| Self::derive_value(base, r, self.reference(key, fallback));
        NineKeys {
            dock_radius: base.clamp(0, 48),
            dock_inner_radius: d("DockInnerRadius", 19),
            control_radius: d("ControlRadius", 12),
            popup_radius: d("PopupRadius", 12),
            widget_radius: d("WidgetRadius", 12),
            dock_widget_radius: d("DockWidgetRadius", 12),
            dock_widget_action_radius: d("DockWidgetActionRadius", 5),
            widget_field_radius: d("WidgetFieldRadius", 7),
            widget_quick_action_radius: d("WidgetQuickActionRadius", 18),
        }
    }

    /// The sheen follows the same concentric rule but rounds only the top corners.
    pub fn dock_sheen_radius(&self, base: i32) -> CornerRadius {
        let value = Self::derive_value(
            base,
            self.reference_dock_radius,
            self.reference("DockSheenRadius", Self::SHEEN_REFERENCE),
        );
        CornerRadius {
            top_left: value,
            top_right: value,
            bottom_right: 0,
            bottom_left: 0,
        }
    }

    /// Rebuilds all keys, including `DockSheenRadius`.
    pub fn rebuild(&self) -> std::collections::BTreeMap<String, CornerRadius> {
        let keys = self.derive_for(self.base);
        let mut out: std::collections::BTreeMap<String, CornerRadius> = keys
            .map()
            .into_iter()
            .map(|(key, value)| (key.to_owned(), CornerRadius::uniform(value)))
            .collect();
        out.insert(
            "DockSheenRadius".to_owned(),
            self.dock_sheen_radius(self.base),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concentric_rule_matches_csharp() {
        assert_eq!(RadiusManager::derive_value(20, 20, 19), 19);
        assert_eq!(RadiusManager::derive_value(24, 20, 19), 23);
        assert_eq!(RadiusManager::derive_value(8, 20, 12), 0);
        let keys = RadiusManager::derive(24);
        assert_eq!(keys.dock_radius, 24);
        assert_eq!(keys.dock_inner_radius, 23);
        assert_eq!(keys.control_radius, 16);
        assert_eq!(keys.dock_widget_action_radius, 9);
        assert_eq!(keys.get("PopupRadius"), Some(16));
        assert_eq!(keys.get("Unknown"), None);
    }

    #[test]
    fn pack_references_shift_the_hierarchy() {
        let refs = RadiusReferences::from([
            ("DockRadius".to_owned(), 18),
            ("ControlRadius".to_owned(), 10),
            ("DockSheenRadius".to_owned(), 17),
        ]);
        let manager = RadiusManager::with_references(refs);
        let keys = manager.derive_for(18);
        assert_eq!(keys.dock_radius, 18);
        assert_eq!(keys.control_radius, 10);
        assert_eq!(
            manager.dock_sheen_radius(18),
            CornerRadius {
                top_left: 17,
                top_right: 17,
                bottom_right: 0,
                bottom_left: 0,
            }
        );
        let map = manager.rebuild();
        assert_eq!(map.len(), 10);
    }
}
