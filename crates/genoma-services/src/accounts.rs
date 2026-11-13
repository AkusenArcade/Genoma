//! The people on the machine, as AccountsService keeps them
//! (`@genoma/accounts`): this session's user, with the name and picture a
//! lock screen or a session menu shows, and the people a greeter offers.
//!
//! Read whole at the start and again after any of AccountsService's
//! signals. Without AccountsService, this user is still known, from
//! `/etc/passwd` (its GECOS name) and `~/.face`, and nothing can be set.

use std::{cell::RefCell, collections::HashMap, time::Duration};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{Signal, batch};
use zbus::{MatchRule, MessageStream, OwnedObjectPath, message::Type, zvariant::OwnedVariant};

use crate::bus::{self, Bus};

const ACCOUNTS: &str = "org.freedesktop.Accounts";
const USER: &str = "org.freedesktop.Accounts.User";
const SETTLE: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct User {
    pub uid: u32,
    pub login: String,
    /// The real name, or the login when there is none.
    pub name: String,
    /// A picture that can be read, or none.
    pub icon: Option<String>,
    pub email: String,
    pub language: String,
    pub administrator: bool,
    pub locked: bool,
    pub home: String,
    pub shell: String,
    /// How many times they logged in: the greeter's order.
    pub logins: u64,
}

pub enum Update {
    Available(bool),
    /// This user, and the people a greeter offers.
    State(Option<User>, Vec<User>),
    Error(String),
}

pub struct Accounts {
    /// Whether AccountsService answers: without it nothing can be set.
    pub available: Signal<bool>,
    pub me: Signal<User>,
    pub users: Signal<Vec<User>>,
    /// The last change's failure, or "".
    pub error: Signal<String>,
    bus: RefCell<Option<(Bus, Sender<Update>)>>,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts {
            available: Signal::new(false),
            me: Signal::new(from_passwd(uid())),
            users: Signal::new(Vec::new()),
            error: Signal::new(String::new()),
            bus: RefCell::new(None),
        }
    }
}

fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// The first picture that can be read and is not empty.
fn readable(candidates: &[String]) -> Option<String> {
    candidates.iter().find(|p| std::fs::File::open(p).is_ok() && std::fs::metadata(p).is_ok_and(|m| m.len() > 0)).cloned()
}

/// One `/etc/passwd` line for `uid`, as a user.
pub fn passwd_entry(text: &str, uid: u32) -> Option<User> {
    text.lines().find_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 || f[2].parse::<u32>().ok()? != uid {
            return None;
        }
        let gecos = f[4].split(',').next().unwrap_or("").trim();
        let login = f[0].to_owned();
        Some(User {
            uid,
            name: if gecos.is_empty() { login.clone() } else { gecos.to_owned() },
            login,
            home: f[5].to_owned(),
            shell: f[6].to_owned(),
            ..User::default()
        })
    })
}

fn from_passwd(uid: u32) -> User {
    let mut me = std::fs::read_to_string("/etc/passwd").ok().and_then(|t| passwd_entry(&t, uid)).unwrap_or(User { uid, ..User::default() });
    if me.home.is_empty() {
        me.home = std::env::var("HOME").unwrap_or_default();
    }
    me.icon = readable(&[format!("{}/.face", me.home), format!("{}/.face.icon", me.home)]);
    me
}

type Props = HashMap<String, OwnedVariant>;

fn get<T: TryFrom<OwnedVariant>>(props: &Props, key: &str) -> Option<T> {
    props.get(key).and_then(|v| T::try_from(v.clone()).ok())
}

fn text(props: &Props, key: &str) -> String {
    get::<String>(props, key).unwrap_or_default()
}

/// A user as AccountsService describes one; the picture falls back to the
/// home's `.face` as the desktops do.
pub fn user_of(props: &Props) -> User {
    let login = text(props, "UserName");
    let name = text(props, "RealName");
    let home = text(props, "HomeDirectory");
    let mut icons = vec![text(props, "IconFile")];
    if !home.is_empty() {
        icons.push(format!("{home}/.face"));
        icons.push(format!("{home}/.face.icon"));
    }
    icons.retain(|p| !p.is_empty());
    User {
        uid: get::<u64>(props, "Uid").unwrap_or(0) as u32,
        name: if name.trim().is_empty() { login.clone() } else { name.trim().to_owned() },
        login,
        icon: readable(&icons),
        email: text(props, "Email"),
        language: text(props, "Language"),
        administrator: get::<i32>(props, "AccountType") == Some(1),
        locked: get(props, "Locked").unwrap_or(false),
        home,
        shell: text(props, "Shell"),
        logins: get(props, "LoginFrequency").unwrap_or(0),
    }
}

async fn read(conn: &zbus::Connection, me: u32) -> zbus::Result<Update> {
    let manager = zbus::Proxy::new(conn, ACCOUNTS, "/org/freedesktop/Accounts", ACCOUNTS).await?;
    let mut paths: Vec<OwnedObjectPath> = manager.call("ListCachedUsers", &()).await?;
    let own: Option<OwnedObjectPath> = manager.call("FindUserById", &(me as i64,)).await.ok();
    if let Some(own) = &own {
        if !paths.contains(own) {
            paths.push(own.clone());
        }
    }
    let mut users = Vec::new();
    let mut mine = None;
    for path in paths {
        let props = zbus::fdo::PropertiesProxy::builder(conn).destination(ACCOUNTS).path(path.clone()).build().await?;
        let all: Props = props.get_all(USER.try_into()?).await?.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let user = user_of(&all);
        let system = get::<bool>(&all, "SystemAccount").unwrap_or(false);
        if user.uid == me {
            mine = Some(user.clone());
        }
        if !system {
            users.push(user);
        }
    }
    users.sort_by(|a, b| b.logins.cmp(&a.logins).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(Update::State(mine, users))
}

impl Accounts {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Available(on) => self.available.set(on),
            Update::State(me, users) => {
                if let Some(me) = me {
                    if self.me.get_untracked().as_ref() != Some(&me) {
                        self.me.set(me);
                    }
                }
                if self.users.get_untracked().as_ref() != Some(&users) {
                    self.users.set(users);
                }
            }
            Update::Error(said) => self.error.set(said),
        });
    }

    pub fn start(&self, bus: &Bus, post: Sender<Update>) {
        *self.bus.borrow_mut() = Some((bus.clone(), post.clone()));
        let pause = bus.clone();
        bus.spawn("accounts", async move {
            let conn = bus::system().await?;
            let rule = MatchRule::builder().msg_type(Type::Signal).sender(ACCOUNTS).build()?;
            let mut stream = pause.drained("accounts signals", MessageStream::for_match_rule(rule, &conn, None).await?);
            match read(&conn, uid()).await {
                Ok(state) => {
                    let _ = post.send(Update::Available(true));
                    let _ = post.send(state);
                }
                // No AccountsService: /etc/passwd stands.
                Err(e) => {
                    log::info!("accounts: {e}");
                    return Ok(());
                }
            }
            while stream.next().await.is_some() {
                pause.sleep(SETTLE).await;
                while let Some(Some(_)) = futures_util::FutureExt::now_or_never(stream.next()) {}
                match read(&conn, uid()).await {
                    Ok(state) => {
                        let _ = post.send(state);
                    }
                    Err(e) => log::warn!("accounts: {e}"),
                }
            }
            Ok(())
        });
    }

    /// One call on this user's AccountsService object; its failure kept in
    /// `error`, its success read back through the signals.
    fn set(&self, method: &'static str, value: String) -> Result<(), String> {
        if !self.available.get_untracked().unwrap_or(false) {
            return Err("AccountsService is not running".into());
        }
        let Some((bus, post)) = self.bus.borrow().clone() else { return Err("accounts: the service has not started".into()) };
        self.error.set(String::new());
        let me = uid();
        bus.spawn("accounts set", async move {
            let conn = bus::system().await?;
            let manager = zbus::Proxy::new(&conn, ACCOUNTS, "/org/freedesktop/Accounts", ACCOUNTS).await?;
            let result = async {
                let path: OwnedObjectPath = manager.call("FindUserById", &(me as i64,)).await?;
                let user = zbus::Proxy::new(&conn, ACCOUNTS, path, USER).await?;
                user.call_method(method, &(value,)).await.map(|_| ())
            }
            .await;
            match result {
                Ok(()) => {
                    // The signal may come before the change is readable.
                    if let Ok(state) = read(&conn, me).await {
                        let _ = post.send(state);
                    }
                }
                Err(e) => {
                    let said = match &e {
                        zbus::Error::MethodError(_, Some(said), _) => said.clone(),
                        other => other.to_string(),
                    };
                    let _ = post.send(Update::Error(said));
                }
            }
            Ok(())
        });
        Ok(())
    }

    /// A new picture: AccountsService copies it where the greeter and the
    /// lock screen read it.
    pub fn set_icon(&self, path: String) -> Result<(), String> {
        self.set("SetIconFile", path)
    }

    pub fn set_name(&self, name: String) -> Result<(), String> {
        self.set("SetRealName", name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwd_gives_the_gecos_name_or_the_login() {
        let text = "root:x:0:0::/root:/bin/bash\nme:x:1000:1000:Ada Lovelace,,,:/home/me:/bin/fish\nyou:x:1001:1001::/home/you:/bin/sh\n";
        let me = passwd_entry(text, 1000).unwrap();
        assert_eq!((me.login.as_str(), me.name.as_str(), me.home.as_str(), me.shell.as_str()), ("me", "Ada Lovelace", "/home/me", "/bin/fish"));
        assert_eq!(passwd_entry(text, 1001).unwrap().name, "you");
        assert!(passwd_entry(text, 4242).is_none());
    }

    #[test]
    fn an_accountsservice_user() {
        let v = |x: zbus::Variant<'static>| OwnedVariant::try_from(x).unwrap();
        let props: Props = [
            ("UserName", v("ada".into())),
            ("RealName", v(" ".into())),
            ("Uid", v(1000u64.into())),
            ("AccountType", v(1i32.into())),
            ("IconFile", v("/nonexistent/ada".into())),
            ("HomeDirectory", v("/nonexistent".into())),
            ("LoginFrequency", v(7u64.into())),
        ]
        .into_iter()
        .map(|(k, x)| (k.to_owned(), x))
        .collect();
        let u = user_of(&props);
        assert_eq!((u.uid, u.name.as_str(), u.administrator, u.icon, u.logins), (1000, "ada", true, None, 7), "no name is the login; an unreadable picture is none");
    }
}
