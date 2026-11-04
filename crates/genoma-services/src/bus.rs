//! The D-Bus side of the services (PRD §9.1): one thread, `genoma-bus`, runs
//! zbus's runtime and every connection on it, one per bus for the whole
//! process. A service's tasks run there and post plain updates into the main
//! loop through a `calloop` channel; only the main thread sets signals.
//!
//! The thread starts with the first service that needs a bus, and a bus is
//! connected the first time a task asks for it.

use std::{future::Future, sync::OnceLock};

use futures_channel::mpsc::UnboundedReceiver;

use zruntime::{SharedRuntime, lock::OnceCell};

static THREAD: OnceLock<Bus> = OnceLock::new();
static SYSTEM: OnceCell<zbus::Connection> = OnceCell::new();
static SESSION: OnceCell<zbus::Connection> = OnceCell::new();

/// The handle on the bus thread: cheap to clone, and what the main thread
/// spawns a service's work with.
#[derive(Clone)]
pub struct Bus {
    runtime: SharedRuntime,
}

impl Bus {
    /// The process's bus thread, started on the first call.
    pub fn shared() -> std::io::Result<Bus> {
        if let Some(bus) = THREAD.get() {
            return Ok(bus.clone());
        }
        let bus = Bus::start()?;
        Ok(THREAD.get_or_init(|| bus).clone())
    }

    fn start() -> std::io::Result<Bus> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("genoma-bus".into()).spawn(move || {
            // The runtime of this thread: connections built in its tasks
            // run on it, since zbus takes the runtime of the thread it is
            // built on.
            let runtime = match SharedRuntime::current() {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = sender.send(Err(e));
                    return;
                }
            };
            let _ = sender.send(Ok(runtime.clone()));
            runtime.block_on(std::future::pending::<()>());
        })?;
        let runtime = receiver.recv().map_err(|_| std::io::Error::other("the bus thread did not start"))??;
        Ok(Bus { runtime })
    }

    /// A pause on the bus thread, for a task gathering a burst of signals.
    pub fn sleep(&self, duration: std::time::Duration) -> impl Future<Output = ()> + Send + 'static {
        let sleep = self.runtime.sleep(duration);
        async move {
            sleep.await;
        }
    }

    /// Runs `work` on the bus thread until it ends or the task is dropped.
    pub fn task<T: Send + 'static>(&self, name: &'static str, work: impl Future<Output = T> + Send + 'static) -> zruntime::Task<T, zruntime::Shared> {
        self.runtime.spawn(name, work)
    }

    /// `stream`, emptied as fast as messages arrive, so that it never holds
    /// up the connection it belongs to.
    ///
    /// zbus gives each match rule a queue of 64, and while one is full the
    /// connection reads nothing more: no signal, and no reply to a call. A
    /// service that reads its state with calls between two polls of its
    /// stream waits for a reply that cannot come once a burst outgrows the
    /// queue, and the whole connection stops for the rest of the session.
    /// NetworkManager's burst on resume, or a scan among many access points,
    /// did that to the system bus: the Wi-Fi switch and the networks went
    /// deaf. Here a task of its own does nothing but move the messages into
    /// an unbounded channel, which the service polls instead.
    pub fn drained<S>(&self, name: &'static str, mut stream: S) -> UnboundedReceiver<S::Item>
    where
        S: futures_util::Stream + Unpin + Send + 'static,
        S::Item: Send + 'static,
    {
        use futures_util::StreamExt;
        let (sender, receiver) = futures_channel::mpsc::unbounded();
        self.runtime
            .spawn(name, async move {
                while let Some(message) = stream.next().await {
                    if sender.unbounded_send(message).is_err() {
                        break;
                    }
                }
            })
            .detach();
        receiver
    }

    /// Runs `work` on the bus thread. A failure is logged under `name`: a
    /// service whose daemon is missing leaves its signals at their defaults.
    pub fn spawn(&self, name: &'static str, work: impl Future<Output = zbus::Result<()>> + Send + 'static) {
        self.runtime
            .spawn(name, async move {
                if let Err(e) = work.await {
                    log::warn!("{name}: {e}");
                }
            })
            .detach();
    }
}

pub async fn system() -> zbus::Result<zbus::Connection> {
    SYSTEM.get_or_try_init(zbus::Connection::system).await.cloned()
}

pub async fn session() -> zbus::Result<zbus::Connection> {
    SESSION.get_or_try_init(zbus::Connection::session).await.cloned()
}

/// Returns once `name` has no owner on `conn`'s bus: at once when it has
/// none now, else when its owner lets it go. For a name a session has one
/// owner of, held by another shell that is on its way out (the switch from
/// Bioma on Quickshell): taken when it is free, not given up for good.
pub async fn until_free(conn: &zbus::Connection, name: &str) -> zbus::Result<()> {
    use futures_util::StreamExt;
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let mut changes = dbus.receive_name_owner_changed_with_args(&[(0, name)]).await?;
    if !dbus.name_has_owner(zbus::names::BusName::try_from(name)?).await? {
        return Ok(());
    }
    while let Some(change) = changes.next().await {
        if change.args()?.new_owner().is_none() {
            return Ok(());
        }
    }
    Err(zbus::Error::Failure(format!("{name}: the bus stopped saying who owns it")))
}
