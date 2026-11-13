//! The desktop portal (`@genoma/portal`, xdg-desktop-portal on the session
//! bus): what the desktop asks of every application, read the way the
//! applications read it, so a shell follows the same preferences they do.
//!
//! Settings: `org.freedesktop.appearance` — the colour scheme, the accent
//! colour, high contrast and reduced motion — read once and followed on
//! `SettingChanged`. Nothing polls.

use std::collections::HashMap;

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::zvariant::OwnedVariant;

use crate::bus::{self, Bus};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";

/// What the desktop prefers, as the portal says it.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    /// "dark", "light", or "default" (no preference).
    pub color_scheme: &'static str,
    /// "#rrggbb", or none where the desktop sets no accent.
    pub accent: Option<String>,
    pub high_contrast: bool,
    pub reduced_motion: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { color_scheme: "default", accent: None, high_contrast: false, reduced_motion: false }
    }
}

pub enum Update {
    Available(bool),
    Setting(String, OwnedVariant),
}

pub struct Portal {
    /// Whether a portal answers on the session bus.
    pub available: Signal<bool>,
    pub appearance: Signal<Appearance>,
}

impl Default for Portal {
    fn default() -> Self {
        Portal { available: Signal::new(false), appearance: Signal::new(Appearance::default()) }
    }
}

/// One `org.freedesktop.appearance` key into the model; others are left.
pub fn set(appearance: &mut Appearance, key: &str, value: &OwnedVariant) {
    match key {
        "color-scheme" => {
            appearance.color_scheme = match u32::try_from(value.clone()) {
                Ok(1) => "dark",
                Ok(2) => "light",
                _ => "default",
            }
        }
        // Three doubles 0…1; any outside means no accent.
        "accent-color" => {
            appearance.accent = <(f64, f64, f64)>::try_from(value.clone()).ok().and_then(|(r, g, b)| {
                let inside = |c: f64| (0.0..=1.0).contains(&c);
                let byte = |c: f64| (c * 255.0).round() as u8;
                (inside(r) && inside(g) && inside(b)).then(|| format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b)))
            })
        }
        "contrast" => appearance.high_contrast = u32::try_from(value.clone()) == Ok(1),
        "reduced-motion" => appearance.reduced_motion = u32::try_from(value.clone()) == Ok(1),
        _ => {}
    }
}

impl Portal {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Available(on) => self.available.set(on),
            Update::Setting(key, value) => {
                let old = self.appearance.get_untracked().unwrap_or_default();
                let mut new = old.clone();
                set(&mut new, &key, &value);
                if new != old {
                    self.appearance.set(new);
                }
            }
        });
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        let pause = bus.clone();
        bus.spawn("portal settings", async move {
            let conn = bus::session().await?;
            let settings = zbus::Proxy::new(&conn, PORTAL, PATH, SETTINGS).await?;
            // Subscribed first, so nothing changes unseen between the two.
            let changes = pause.drained("portal setting changes", settings.receive_signal("SettingChanged").await?);
            let all: HashMap<String, HashMap<String, OwnedVariant>> = match settings.call("ReadAll", &(vec![APPEARANCE],)).await {
                Ok(all) => all,
                Err(e) => {
                    log::info!("portal: {e}");
                    return Ok(());
                }
            };
            let _ = post.send(Update::Available(true));
            for (key, value) in all.into_iter().filter(|(ns, _)| ns == APPEARANCE).flat_map(|(_, keys)| keys) {
                let _ = post.send(Update::Setting(key, value));
            }
            let mut changes = changes;
            while let Some(message) = changes.next().await {
                if let Ok((namespace, key, value)) = message.body().deserialize::<(String, String, OwnedVariant)>() {
                    if namespace == APPEARANCE {
                        let _ = post.send(Update::Setting(key, value));
                    }
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_keys() {
        let v = |x: zbus::Variant<'static>| OwnedVariant::try_from(x).unwrap();
        let mut a = Appearance::default();
        set(&mut a, "color-scheme", &v(1u32.into()));
        set(&mut a, "accent-color", &v((0.227451f64, 0.580392f64, 0.290196f64).into()));
        set(&mut a, "contrast", &v(1u32.into()));
        assert_eq!(a, Appearance { color_scheme: "dark", accent: Some("#3a944a".into()), high_contrast: true, reduced_motion: false });
        set(&mut a, "accent-color", &v((-1.0f64, 0.0f64, 0.0f64).into()));
        set(&mut a, "color-scheme", &v(0u32.into()));
        assert_eq!((a.accent, a.color_scheme), (None, "default"), "outside 0…1 is no accent");
    }
}
