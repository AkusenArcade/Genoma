//! `g.pam` (PRD §9.1): an authentication from Luau, as Quickshell's
//! `PamContext`: start it, answer its prompts with `respond`, and hear how it
//! ended. PAM runs on a thread of its own; its events reach the interface
//! through the same channel into the loop `g.dbus` uses.

use std::{rc::Rc, sync::mpsc};

use genoma_services::pam::{Event, Outcome, authenticate};
use mlua::{Function, Table, UserData, UserDataMethods, Value as LuaValue};

use crate::dbus::{Dbus, Delivery, Dyn};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

pub struct Auth {
    answers: mpsc::Sender<Option<String>>,
}

impl UserData for Auth {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // auth:respond(text): the answer to the prompt PAM is waiting on.
        methods.add_method("respond", |_, this, answer: String| {
            let _ = this.answers.send(Some(answer));
            Ok(())
        });
        // auth:cancel(): ends it at the prompt it waits on.
        methods.add_method("cancel", |_, this, ()| {
            let _ = this.answers.send(None);
            Ok(())
        });
    }
}

/// `g.pam.start { service = "bioma-lock", directory = "~/…/assets/pam",
/// user = …, on_prompt = function(message, echo) … end, on_message =
/// function(message, error) … end, on_done = function(result) … end }`.
/// `result` is "success", "failure", "max-tries" or "error: …".
pub fn start(inbox: &Rc<Dbus>, t: Table) -> mlua::Result<Auth> {
    let service: String = t.get("service")?;
    let directory = t.get::<Option<String>>("directory")?.map(|d| crate::io::expand(&d).display().to_string());
    let user = match t.get::<Option<String>>("user")? {
        Some(u) => u,
        None => std::env::var("USER").map_err(|_| err("g.pam: no user, and no USER"))?,
    };
    let (on_prompt, on_message, on_done): (Option<Function>, Option<Function>, Option<Function>) =
        (t.get("on_prompt")?, t.get("on_message")?, t.get("on_done")?);
    let lua = t.weak_lua().upgrade();
    // One callback for the channel, sorting the events to the interface's.
    let route = lua.create_function(move |_, (kind, a, b): (String, LuaValue, LuaValue)| {
        let f = match kind.as_str() {
            "prompt" => &on_prompt,
            "message" => &on_message,
            _ => &on_done,
        };
        match f {
            Some(f) => f.call::<()>((a, b)),
            None => Ok(()),
        }
    })?;
    let (id, out) = inbox.listen(route);
    let (answers, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("genoma-pam".into())
        .spawn(move || {
            let mut events = |event: Event| {
                let (values, last) = match event {
                    Event::Prompt { message, echo } => (vec![Dyn::Str("prompt".into()), Dyn::Str(message), Dyn::Bool(echo)], false),
                    Event::Message { message, error } => {
                        (vec![Dyn::Str("message".into()), Dyn::Str(message), Dyn::Bool(error)], false)
                    }
                    Event::Done(outcome) => (vec![Dyn::Str("done".into()), Dyn::Str(outcome.name()), Dyn::Bool(outcome == Outcome::Success)], true),
                };
                let _ = out.send(Delivery { id, result: Ok(values), last });
            };
            authenticate(&service, directory.as_deref(), &user, &mut events, &receiver);
        })
        .map_err(|e| err(format!("g.pam: no thread: {e}")))?;
    Ok(Auth { answers })
}
