//! Polkit agent (PRD §9.1): what asks for a password when a program wants a
//! privilege, as Bioma's `services/Polkit.qml` does with Quickshell's
//! `PolkitAgent`.
//!
//! Genoma serves `org.freedesktop.PolicyKit1.AuthenticationAgent` on the
//! system bus and registers it with polkit for a subject: the session, as a
//! shell's agent is, or one process, as `pkttyagent --process` is (what a
//! test uses, so the session's agent is left alone). A request is shown to
//! the interface; the password it gives is checked by polkit's own helper,
//! through its socket (polkit 126 and later) or the setuid program, and the
//! helper tells polkit the outcome. A wrong password asks again.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
};

use calloop::channel::Sender;
use genoma_core::reactive::{Signal, batch};
use zbus::{OwnedVariant, Variant};

use crate::bus::{self, Bus};

const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AGENT_PATH: &str = "/org/genoma/PolkitAgent";
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";
const HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

/// One authentication polkit asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    pub cookie: String,
    pub action_id: String,
    /// polkit's message for the person: "Authentication is needed to …".
    pub message: String,
    pub icon_name: String,
    /// Whose password is asked for.
    pub user: String,
    /// What the helper asks ("Password: "), and whether to show the answer.
    pub prompt: String,
    pub echo: bool,
    /// The last answer was wrong.
    pub failed: bool,
    /// Something to show from PAM, information or an error.
    pub info: String,
}

pub enum Update {
    Registered(Result<(), String>),
    Request(Request),
    /// The request ended: `true` authenticated, `false` cancelled.
    Done { cookie: String, success: bool },
}

/// What to register for.
#[derive(Clone, Debug)]
pub enum Subject {
    /// The session in `$XDG_SESSION_ID`: a shell's agent.
    Session,
    /// One process: an agent for that process alone.
    Process(u32),
}

type Answers = Arc<Mutex<HashMap<String, mpsc::Sender<Option<String>>>>>;

pub struct Polkit {
    pub registered: Signal<bool>,
    pub error: Signal<String>,
    /// The request being answered, if any.
    pub request: Signal<Option<Request>>,
    answers: Answers,
}

impl Default for Polkit {
    fn default() -> Self {
        Polkit {
            registered: Signal::new(false),
            error: Signal::new(String::new()),
            request: Signal::new(None),
            answers: Arc::default(),
        }
    }
}

fn user_name(uid: u32) -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // Safety: the buffer outlives the call and `entry` points into it.
    let code = unsafe { libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) };
    if code != 0 || found.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }.to_string_lossy().into_owned())
}

/// The helper's conversation, on a thread of its own: every prompt to the
/// interface and its answer back. `Ok(true)` authenticated, `Ok(false)` a
/// wrong answer, `Err` cancelled or broken.
fn converse(request: &Request, post: &Sender<Update>, answers: &mpsc::Receiver<Option<String>>, failed: bool) -> Result<bool, String> {
    // The socket where polkit 126+ runs the helper for us; the setuid
    // program where it does not. Through the socket the user comes first,
    // as it cannot be an argument; then the cookie, in both.
    let (mut writer, reader): (Box<dyn Write>, Box<dyn BufRead>) = match UnixStream::connect(HELPER_SOCKET) {
        Ok(stream) => {
            let read = stream.try_clone().map_err(|e| e.to_string())?;
            let mut stream: Box<dyn Write> = Box::new(stream);
            writeln!(stream, "{}", request.user).map_err(|e| e.to_string())?;
            (stream, Box::new(BufReader::new(read)))
        }
        Err(_) => {
            let mut child = Command::new(HELPER)
                .arg(&request.user)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| format!("{HELPER}: {e}"))?;
            let stdin = child.stdin.take().ok_or("no stdin")?;
            let stdout = child.stdout.take().ok_or("no stdout")?;
            (Box::new(stdin), Box::new(BufReader::new(stdout)))
        }
    };
    writeln!(writer, "{}", request.cookie).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    let mut info = String::new();
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        let (kind, text) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match kind {
            "PAM_PROMPT_ECHO_OFF" | "PAM_PROMPT_ECHO_ON" => {
                let shown = Request {
                    prompt: text.to_owned(),
                    echo: kind == "PAM_PROMPT_ECHO_ON",
                    failed,
                    info: std::mem::take(&mut info),
                    ..request.clone()
                };
                let _ = post.send(Update::Request(shown));
                let Ok(Some(answer)) = answers.recv() else { return Err("cancelled".into()) };
                writeln!(writer, "{answer}").map_err(|e| e.to_string())?;
                writer.flush().map_err(|e| e.to_string())?;
            }
            "PAM_ERROR_MSG" | "PAM_TEXT_INFO" => info = text.to_owned(),
            "SUCCESS" => return Ok(true),
            "FAILURE" => return Ok(false),
            _ => {}
        }
    }
    Err("the helper ended without an answer".into())
}

struct Agent {
    post: Sender<Update>,
    answers: Answers,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedVariant>)>,
    ) -> zbus::fdo::Result<()> {
        // The person's own identity when it is among those that may answer
        // (an administrator in wheel), else the first.
        let me = unsafe { libc::getuid() };
        let uids: Vec<u32> = identities
            .iter()
            .filter(|(kind, _)| kind == "unix-user")
            .filter_map(|(_, d)| d.get("uid").and_then(|v| u32::try_from(v.clone()).ok()))
            .collect();
        let uid = uids.iter().copied().find(|u| *u == me).or(uids.first().copied());
        let Some(user) = uid.and_then(user_name) else {
            return Err(zbus::fdo::Error::Failed("no identity to authenticate as".into()));
        };
        let request = Request { cookie: cookie.clone(), action_id, message, icon_name, user, ..Request::default() };
        let (answers_in, answers_out) = mpsc::channel();
        self.answers.lock().unwrap().insert(cookie.clone(), answers_in);
        let (done_in, done_out) = futures_channel::oneshot::channel();
        let post = self.post.clone();
        std::thread::Builder::new()
            .name("genoma-polkit".into())
            .spawn(move || {
                let mut failed = false;
                let result = loop {
                    match converse(&request, &post, &answers_out, failed) {
                        Ok(true) => break Ok(()),
                        // Wrong: ask again, saying so.
                        Ok(false) => failed = true,
                        Err(e) => break Err(e),
                    }
                };
                let _ = post.send(Update::Done { cookie: request.cookie.clone(), success: result.is_ok() });
                let _ = done_in.send(result);
            })
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let result = done_out.await.unwrap_or(Err("the agent's thread died".into()));
        self.answers.lock().unwrap().remove(&cookie);
        result.map_err(|e| zbus::fdo::Error::Failed(e))
    }

    async fn cancel_authentication(&self, cookie: String) {
        if let Some(answers) = self.answers.lock().unwrap().get(&cookie) {
            let _ = answers.send(None);
        }
    }
}

/// A process's start time, as polkit identifies a process by it.
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

impl Polkit {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Registered(Ok(())) => self.registered.set(true),
            Update::Registered(Err(e)) => self.error.set(e),
            Update::Request(r) => self.request.set(Some(r)),
            Update::Done { cookie, .. } => {
                if self.request.get_untracked().flatten().is_some_and(|r| r.cookie == cookie) {
                    self.request.set(None);
                }
            }
        });
    }

    /// Serves the agent and registers it for `subject`.
    pub fn start(&self, bus: &Bus, post: Sender<Update>, subject: Subject) {
        let answers = self.answers.clone();
        bus.spawn("polkit", async move {
            let result: zbus::Result<()> = async {
                let conn = bus::system().await?;
                conn.object_server().at(AGENT_PATH, Agent { post: post.clone(), answers }).await?;
                let (kind, details): (&str, HashMap<&str, Variant<'_>>) = match subject {
                    Subject::Session => {
                        let id = std::env::var("XDG_SESSION_ID").map_err(|_| zbus::Error::Failure("no XDG_SESSION_ID".into()))?;
                        ("unix-session", HashMap::from([("session-id", Variant::from(id))]))
                    }
                    Subject::Process(pid) => {
                        let start = start_time(pid).ok_or_else(|| zbus::Error::Failure(format!("no process {pid}")))?;
                        ("unix-process", HashMap::from([("pid", Variant::from(pid)), ("start-time", Variant::from(start))]))
                    }
                };
                let authority = zbus::Proxy::new(&conn, AUTHORITY, AUTHORITY_PATH, "org.freedesktop.PolicyKit1.Authority").await?;
                let locale = std::env::var("LANG").unwrap_or_else(|_| "C".into());
                authority.call_method("RegisterAuthenticationAgent", &((kind, details), locale.as_str(), AGENT_PATH)).await?;
                Ok(())
            }
            .await;
            let _ = post.send(Update::Registered(result.map_err(|e| e.to_string())));
            Ok(())
        });
    }

    /// The answer to the request's prompt.
    pub fn respond(&self, cookie: &str, answer: String) {
        if let Some(answers) = self.answers.lock().unwrap().get(cookie) {
            let _ = answers.send(Some(answer));
        }
    }

    pub fn cancel(&self, cookie: &str) {
        if let Some(answers) = self.answers.lock().unwrap().get(cookie) {
            let _ = answers.send(None);
        }
    }
}
