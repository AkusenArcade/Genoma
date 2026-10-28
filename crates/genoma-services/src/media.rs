//! Media (PRD §9.1): the MPRIS players on the session bus, as Bioma's
//! `services/Media.qml` reads them from `Quickshell.Services.Mpris`, with
//! the transport actions.
//!
//! Players are found with `ListNames` and then follow `NameOwnerChanged`; a
//! player is read again whenever it says its properties changed. MPRIS does
//! not push the position, so it is read when the track, the state or the rate
//! change and when the player says it seeked, and between those it is the
//! last reading carried forward at the playback rate. Nothing polls.

use std::{cell::RefCell, collections::HashMap, time::Instant};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{MatchRule, MessageStream, OwnedVariant, message::Type, proxy::CacheProperties, wire::OwnedObjectPath};

use crate::bus::{self, Bus};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";

#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    /// Its bus name, `org.mpris.MediaPlayer2.firefox.instance_1_324`: what
    /// an action names it by.
    pub name: String,
    pub identity: String,
    pub desktop_entry: String,
    /// "playing", "paused" or "stopped".
    pub status: &'static str,
    pub title: String,
    /// The artists, joined with ", " as Quickshell joins them.
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub art_url: String,
    /// Seconds; 0 when the player does not say.
    pub length: f64,
    pub track_id: String,
    pub rate: f64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
    pub can_control: bool,
    /// Whether the player answers its position at all.
    pub position_supported: bool,
    /// The position when it was last read, seconds, and when.
    pub position: f64,
    pub position_at: Instant,
}

impl Player {
    /// The position now: the last reading carried forward at the rate while
    /// playing, never past the length.
    pub fn position_now(&self) -> f64 {
        let mut p = self.position;
        if self.status == "playing" {
            p += self.position_at.elapsed().as_secs_f64() * self.rate;
        }
        if self.length > 0.0 { p.clamp(0.0, self.length) } else { p.max(0.0) }
    }
}

pub enum Update {
    Player(Box<Player>),
    Seeked { name: String, position: f64, at: Instant },
    Gone(String),
}

pub struct Media {
    /// In the order they appeared.
    pub players: Signal<Vec<Player>>,
    bus: RefCell<Option<Bus>>,
}

impl Default for Media {
    fn default() -> Self {
        Media { players: Signal::new(Vec::new()), bus: RefCell::new(None) }
    }
}

#[zbus::proxy(interface = "org.mpris.MediaPlayer2", default_path = "/org/mpris/MediaPlayer2")]
trait Root {
    #[zbus(property)]
    fn identity(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn desktop_entry(&self) -> zbus::Result<String>;
}

#[zbus::proxy(interface = "org.mpris.MediaPlayer2.Player", default_path = "/org/mpris/MediaPlayer2")]
trait MprisPlayer {
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;
    #[zbus(property)]
    fn position(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn rate(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn can_play(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_pause(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_go_next(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_go_previous(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_seek(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_control(&self) -> zbus::Result<bool>;
    fn play_pause(&self) -> zbus::Result<()>;
    fn play(&self) -> zbus::Result<()>;
    fn pause(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    fn seek(&self, offset: i64) -> zbus::Result<()>;
    fn set_position(&self, track: &OwnedObjectPath, position: i64) -> zbus::Result<()>;
}

fn text(m: &HashMap<String, OwnedVariant>, key: &str) -> String {
    m.get(key).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default()
}

/// A string or a list of them (`xesam:artist` is a list), joined.
fn names(m: &HashMap<String, OwnedVariant>, key: &str) -> String {
    let Some(v) = m.get(key) else { return String::new() };
    if let Ok(list) = Vec::<String>::try_from(v.clone()) {
        return list.join(", ");
    }
    String::try_from(v.clone()).unwrap_or_default()
}

/// `mpris:length` is an int64 by the specification, and a uint64 or a
/// double from some players.
fn micros(m: &HashMap<String, OwnedVariant>, key: &str) -> i64 {
    let Some(v) = m.get(key) else { return 0 };
    i64::try_from(v.clone())
        .or_else(|_| u64::try_from(v.clone()).map(|u| u as i64))
        .or_else(|_| f64::try_from(v.clone()).map(|f| f as i64))
        .unwrap_or(0)
}

fn track(m: &HashMap<String, OwnedVariant>) -> String {
    let Some(v) = m.get("mpris:trackid") else { return String::new() };
    OwnedObjectPath::try_from(v.clone())
        .map(|p| p.as_str().to_owned())
        .or_else(|_| String::try_from(v.clone()))
        .unwrap_or_default()
}

async fn read(conn: &zbus::Connection, name: &str) -> zbus::Result<Player> {
    let root = RootProxy::builder(conn).destination(name.to_owned()).cache_properties(CacheProperties::No).build().await?;
    let player =
        MprisPlayerProxy::builder(conn).destination(name.to_owned()).cache_properties(CacheProperties::No).build().await?;
    let m = player.metadata().await.unwrap_or_default();
    let position = player.position().await;
    let length = micros(&m, "mpris:length");
    Ok(Player {
        name: name.to_owned(),
        identity: root.identity().await.unwrap_or_default(),
        desktop_entry: root.desktop_entry().await.unwrap_or_default(),
        status: match player.playback_status().await.unwrap_or_default().as_str() {
            "Playing" => "playing",
            "Paused" => "paused",
            _ => "stopped",
        },
        title: text(&m, "xesam:title"),
        artist: names(&m, "xesam:artist"),
        album: text(&m, "xesam:album"),
        album_artist: names(&m, "xesam:albumArtist"),
        art_url: text(&m, "mpris:artUrl"),
        length: if length > 0 { length as f64 / 1e6 } else { 0.0 },
        track_id: track(&m),
        rate: player.rate().await.unwrap_or(1.0),
        can_play: player.can_play().await.unwrap_or(false),
        can_pause: player.can_pause().await.unwrap_or(false),
        can_go_next: player.can_go_next().await.unwrap_or(false),
        can_go_previous: player.can_go_previous().await.unwrap_or(false),
        can_seek: player.can_seek().await.unwrap_or(false),
        can_control: player.can_control().await.unwrap_or(false),
        position_supported: position.is_ok(),
        position: position.map(|p| p as f64 / 1e6).unwrap_or(0.0),
        position_at: Instant::now(),
    })
}

impl Media {
    pub fn apply(&self, update: Update) {
        batch(|| {
            let mut players = self.players.get_untracked().unwrap_or_default();
            match update {
                Update::Player(p) => match players.iter_mut().find(|q| q.name == p.name) {
                    Some(q) => *q = *p,
                    None => players.push(*p),
                },
                Update::Seeked { name, position, at } => {
                    if let Some(q) = players.iter_mut().find(|q| q.name == name) {
                        q.position = position;
                        q.position_at = at;
                    }
                }
                Update::Gone(name) => players.retain(|q| q.name != name),
            }
            self.players.set(players);
        });
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some(bus.clone());
        bus.spawn("media", async move {
            let conn = bus::session().await?;
            // The players' own signals, by sender, and the bus's word on
            // names coming and going.
            let players_rule = MatchRule::builder().msg_type(Type::Signal).path(PATH).build()?;
            let names_rule = MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")
                .member("NameOwnerChanged")
                .build()?;
            let players_stream = MessageStream::for_match_rule(players_rule, &conn, None).await?;
            let names_stream = MessageStream::for_match_rule(names_rule, &conn, None).await?;
            let mut signals = futures_util::stream::select(players_stream, names_stream);

            let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
            // Unique name → bus name: a player's signals come from its
            // unique name.
            let mut owners: HashMap<String, String> = HashMap::new();
            for name in dbus.list_names().await? {
                let name = name.to_string();
                if !name.starts_with(PREFIX) {
                    continue;
                }
                if let Ok(owner) = dbus.get_name_owner(name.as_str().try_into()?).await {
                    owners.insert(owner.to_string(), name.clone());
                }
                if let Ok(p) = read(&conn, &name).await {
                    let _ = post.send(Update::Player(Box::new(p)));
                }
            }

            while let Some(message) = signals.next().await {
                let Ok(message) = message else { continue };
                let header = message.header();
                let member = header.member().map(|m| m.as_str().to_owned()).unwrap_or_default();
                if member == "NameOwnerChanged" {
                    let Ok((name, old, new)) = message.body().deserialize::<(String, String, String)>() else { continue };
                    if !name.starts_with(PREFIX) {
                        continue;
                    }
                    owners.remove(&old);
                    if new.is_empty() {
                        let _ = post.send(Update::Gone(name));
                    } else {
                        owners.insert(new, name.clone());
                        if let Ok(p) = read(&conn, &name).await {
                            let _ = post.send(Update::Player(Box::new(p)));
                        }
                    }
                    continue;
                }
                let Some(name) = header.sender().and_then(|s| owners.get(s.as_str())).cloned() else { continue };
                if member == "Seeked" {
                    if let Ok(position) = message.body().deserialize::<i64>() {
                        let at = Instant::now();
                        let _ = post.send(Update::Seeked { name, position: position as f64 / 1e6, at });
                    }
                } else if member == "PropertiesChanged" {
                    if let Ok(p) = read(&conn, &name).await {
                        let _ = post.send(Update::Player(Box::new(p)));
                    }
                }
            }
            Ok(())
        });
    }

    /// One of the transport actions, on the player named `name`.
    pub fn act(&self, name: String, action: Action) {
        let Some(bus) = self.bus.borrow().clone() else {
            log::warn!("media: an action before the service started");
            return;
        };
        bus.spawn("media action", async move {
            let conn = bus::session().await?;
            let player = MprisPlayerProxy::builder(&conn).destination(name.clone()).cache_properties(CacheProperties::No).build().await?;
            match action {
                Action::PlayPause => player.play_pause().await,
                Action::Play => player.play().await,
                Action::Pause => player.pause().await,
                Action::Stop => player.stop().await,
                Action::Next => player.next().await,
                Action::Previous => player.previous().await,
                Action::Seek(by) => player.seek((by * 1e6) as i64).await,
                Action::SetPosition(at) => {
                    // SetPosition wants the current track's id, and does
                    // nothing for another.
                    let m = player.metadata().await?;
                    let track = OwnedObjectPath::try_from(track(&m)).map_err(zbus::Error::from)?;
                    player.set_position(&track, (at * 1e6) as i64).await
                }
            }
        });
    }
}

pub enum Action {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// Seconds, forward or back.
    Seek(f64),
    /// Seconds from the start.
    SetPosition(f64),
}
