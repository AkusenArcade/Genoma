//! PAM (PRD §9.1, §12 question 4): a direct binding to libpam, the five
//! functions and the conversation callback a lock screen needs, rather than a
//! crate unreleased since 2022.
//!
//! An authentication blocks (a wrong password is followed by PAM's delay), so
//! it runs on a thread of its own. What PAM says comes out as [`Event`]s, and
//! a prompt waits on the channel for its answer, so the answer can come from
//! the interface, as Quickshell's `PamContext.respond` does.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr,
    sync::mpsc::Receiver,
};

#[repr(C)]
struct PamMessage {
    style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    retcode: c_int,
}

type Converse = unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Converse,
    appdata: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        confdir: *const c_char,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut c_void, code: c_int) -> *const c_char;
}

const PAM_SUCCESS: c_int = 0;
const PAM_AUTH_ERR: c_int = 7;
const PAM_USER_UNKNOWN: c_int = 10;
const PAM_MAXTRIES: c_int = 11;
const PAM_CONV_ERR: c_int = 19;
const PROMPT_ECHO_OFF: c_int = 1;
const PROMPT_ECHO_ON: c_int = 2;
const ERROR_MSG: c_int = 3;

/// What PAM says while it authenticates.
#[derive(Clone, Debug)]
pub enum Event {
    /// An answer is wanted: `respond` it. `echo` is false for a password.
    Prompt { message: String, echo: bool },
    /// Something to show, an error or information.
    Message { message: String, error: bool },
    /// The end.
    Done(Outcome),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Success,
    /// The answers were wrong.
    Failure,
    /// Too many wrong answers.
    MaxTries,
    /// Anything else: a missing configuration, a module that failed.
    Error(String),
}

impl Outcome {
    pub fn name(&self) -> String {
        match self {
            Outcome::Success => "success".into(),
            Outcome::Failure => "failure".into(),
            Outcome::MaxTries => "max-tries".into(),
            Outcome::Error(e) => format!("error: {e}"),
        }
    }
}

struct Conversation<'a> {
    events: &'a mut dyn FnMut(Event),
    answers: &'a Receiver<Option<String>>,
}

/// PAM's conversation: each message out as an event, and each prompt waits
/// for its answer. A `None` answer (cancelled) ends the conversation.
unsafe extern "C" fn converse(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if count <= 0 || messages.is_null() || responses.is_null() || appdata.is_null() {
        return PAM_CONV_ERR;
    }
    // Safety: PAM passes back the pointer given to pam_start, alive for the
    // whole authentication, and an array of `count` messages.
    let conversation = unsafe { &mut *(appdata as *mut Conversation<'_>) };
    let count = count as usize;
    // Freed by PAM with free(), so allocated with calloc.
    let replies = unsafe { libc::calloc(count, std::mem::size_of::<PamResponse>()) } as *mut PamResponse;
    if replies.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..count {
        let message = unsafe { &**messages.add(i) };
        let text = if message.msg.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(message.msg) }.to_string_lossy().into_owned()
        };
        match message.style {
            PROMPT_ECHO_OFF | PROMPT_ECHO_ON => {
                (conversation.events)(Event::Prompt { message: text, echo: message.style == PROMPT_ECHO_ON });
                let Ok(Some(answer)) = conversation.answers.recv() else {
                    // Cancelled: what was answered so far is freed with it.
                    for j in 0..i {
                        let r = unsafe { &mut *replies.add(j) };
                        if !r.resp.is_null() {
                            unsafe { libc::free(r.resp as *mut c_void) };
                        }
                    }
                    unsafe { libc::free(replies as *mut c_void) };
                    return PAM_CONV_ERR;
                };
                let answer = CString::new(answer).unwrap_or_default();
                unsafe { (*replies.add(i)).resp = libc::strdup(answer.as_ptr()) };
            }
            style => (conversation.events)(Event::Message { message: text, error: style == ERROR_MSG }),
        }
    }
    unsafe { *responses = replies };
    PAM_SUCCESS
}

/// Authenticates `user` with the PAM service `service`, read from
/// `directory` when one is given (Bioma's `assets/pam`) and from /etc/pam.d
/// otherwise. Blocks until PAM is done: run it on a thread of its own.
pub fn authenticate(
    service: &str,
    directory: Option<&str>,
    user: &str,
    events: &mut dyn FnMut(Event),
    answers: &Receiver<Option<String>>,
) {
    let outcome = run(service, directory, user, events, answers);
    events(Event::Done(outcome));
}

fn run(
    service: &str,
    directory: Option<&str>,
    user: &str,
    events: &mut dyn FnMut(Event),
    answers: &Receiver<Option<String>>,
) -> Outcome {
    let (Ok(service), Ok(user)) = (CString::new(service), CString::new(user)) else {
        return Outcome::Error("a name with a NUL in it".into());
    };
    let directory = match directory.map(CString::new) {
        Some(Ok(d)) => Some(d),
        Some(Err(_)) => return Outcome::Error("a directory with a NUL in it".into()),
        None => None,
    };
    let mut conversation = Conversation { events, answers };
    let conv = PamConv { conv: converse, appdata: &mut conversation as *mut Conversation<'_> as *mut c_void };
    let mut handle: *mut c_void = ptr::null_mut();
    // Safety: every pointer outlives the authentication, which ends with
    // pam_end below.
    let started = unsafe {
        pam_start_confdir(
            service.as_ptr(),
            user.as_ptr(),
            &conv,
            directory.as_ref().map_or(ptr::null(), |d| d.as_ptr()),
            &mut handle,
        )
    };
    if started != PAM_SUCCESS || handle.is_null() {
        return Outcome::Error(format!("pam_start: {started}"));
    }
    let mut code = unsafe { pam_authenticate(handle, 0) };
    if code == PAM_SUCCESS {
        // An expired account must not unlock either.
        code = unsafe { pam_acct_mgmt(handle, 0) };
    }
    let outcome = match code {
        PAM_SUCCESS => Outcome::Success,
        PAM_AUTH_ERR | PAM_USER_UNKNOWN => Outcome::Failure,
        PAM_MAXTRIES => Outcome::MaxTries,
        other => {
            let text = unsafe { pam_strerror(handle, other) };
            let text = if text.is_null() {
                format!("PAM error {other}")
            } else {
                unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
            };
            Outcome::Error(text)
        }
    };
    unsafe { pam_end(handle, code) };
    outcome
}
