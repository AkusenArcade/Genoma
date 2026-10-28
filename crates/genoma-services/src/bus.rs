//! The D-Bus side of the services (PRD §9.1): one thread, `genoma-bus`, runs
//! zbus's runtime and every connection on it, one per bus for the whole
//! process. A service's tasks run there and post plain updates into the main
//! loop through a `calloop` channel; only the main thread sets signals.
//!
//! The thread starts with the first service that needs a bus, and a bus is
//! connected the first time a task asks for it.

use std::{future::Future, sync::OnceLock};

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

    /// Runs `work` on the bus thread until it ends or the task is dropped.
    pub fn task<T: Send + 'static>(&self, name: &'static str, work: impl Future<Output = T> + Send + 'static) -> zruntime::Task<T, zruntime::Shared> {
        self.runtime.spawn(name, work)
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
