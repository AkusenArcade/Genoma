//! A call into a running interface from outside it: a keybind asking Bioma
//! to open a cell.
//!
//!     genoma ipc <target> <function> [arguments…]
//!
//! The interface answers with `g.ipc(target, { function = … })`. The socket
//! is `$XDG_RUNTIME_DIR/genoma/<name>.sock`, the name `$GENOMA_IPC` or
//! `genoma`, so two interfaces (the shell and its lock screen) each have
//! their own. A request is its words, each ended by a NUL, and the end of
//! the stream; the reply is one status byte (`0` answered, `1` refused) and
//! the text the function returned.

use std::{
    cell::RefCell,
    io::{ErrorKind, Read, Write},
    net::Shutdown,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use genoma_script::Script;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic};

/// How long a caller has to say what it wants: the loop waits on it.
const PATIENCE: Duration = Duration::from_millis(500);

pub fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let name = std::env::var("GENOMA_IPC").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "genoma".into());
    base.join("genoma").join(format!("{name}.sock"))
}

/// Listens for calls while the interface runs. A socket another instance
/// still answers on is left to it; one nobody answers on is stale, and
/// taken over.
pub fn serve<D: 'static>(handle: &LoopHandle<'static, D>, script: Rc<RefCell<Script>>) {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        log::warn!("ipc: {} is another instance's; this one takes no calls", path.display());
        return;
    }
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("ipc: {}: {e}", path.display());
            return;
        }
    };
    if let Err(e) = listener.set_nonblocking(true) {
        log::warn!("ipc: {e}");
        return;
    }
    log::info!("ipc on {}", path.display());
    let source = Generic::new(listener, Interest::READ, Mode::Level);
    let inserted = handle.insert_source(source, move |_, listener, _| {
        loop {
            match listener.accept() {
                Ok((stream, _)) => answer(stream, &script.borrow()),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    log::warn!("ipc: {e}");
                    break;
                }
            }
        }
        Ok(PostAction::Continue)
    });
    if let Err(e) = inserted {
        log::warn!("ipc: {e}");
    }
}

fn answer(mut stream: UnixStream, script: &Script) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(PATIENCE));
    let mut request = Vec::new();
    if let Err(e) = stream.read_to_end(&mut request) {
        log::warn!("ipc: {e}");
        return;
    }
    let words: Vec<String> =
        request.split(|b| *b == 0).filter(|w| !w.is_empty()).map(|w| String::from_utf8_lossy(w).into_owned()).collect();
    let reply = match words.as_slice() {
        [target, function, args @ ..] => script.ipc(target, function, args),
        _ => Err("a call is: <target> <function> [arguments…]".into()),
    };
    let (status, text) = match reply {
        Ok(text) => (b'0', text),
        Err(text) => (b'1', text),
    };
    let mut out = vec![status];
    out.extend_from_slice(text.as_bytes());
    let _ = stream.write_all(&out);
}

/// `genoma ipc …`: the caller's side. The reply goes to stdout, a refusal
/// to stderr with a failing status.
pub fn call(words: Vec<String>) -> ! {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).unwrap_or_else(|e| {
        eprintln!("genoma ipc: {}: {e}", path.display());
        std::process::exit(2);
    });
    let mut request = Vec::new();
    for w in &words {
        request.extend_from_slice(w.as_bytes());
        request.push(0);
    }
    let mut reply = Vec::new();
    let sent = stream.write_all(&request).and_then(|()| stream.shutdown(Shutdown::Write));
    if let Err(e) = sent.and_then(|()| stream.read_to_end(&mut reply).map(|_| ())) {
        eprintln!("genoma ipc: {e}");
        std::process::exit(2);
    }
    let text = String::from_utf8_lossy(reply.get(1..).unwrap_or_default());
    match reply.first() {
        Some(b'0') => {
            if !text.is_empty() {
                println!("{text}");
            }
            std::process::exit(0)
        }
        _ => {
            eprintln!("genoma ipc: {text}");
            std::process::exit(1)
        }
    }
}
