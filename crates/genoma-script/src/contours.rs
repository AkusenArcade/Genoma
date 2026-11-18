//! `g.contours`: the outlines in a picture, as lines (genoma-scene's
//! `contours`), found on a thread of their own and handed back through
//! the channel into the loop `g.dbus` and `g.pam` use.

use std::rc::Rc;

use genoma_scene::contours::{Ask, contours};
use mlua::{Function, Table};

use crate::dbus::{Dbus, Delivery, Dyn};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// `g.contours { src = path, size = 480, most = 32, shortest = 0.08,
/// on_done = function(lines, width, height) end }`. `lines` is a list of
/// lines, each `{ x1, y1, x2, y2, … }` from 0 to 1 of the picture, longest
/// first; `width` and `height` the picture's own size in pixels. A picture
/// that cannot be read gives `nil`.
pub fn start(inbox: &Rc<Dbus>, t: Table) -> mlua::Result<()> {
    let source: String = t.get("src")?;
    let source = crate::io::expand(&source).display().to_string();
    let base = Ask::default();
    let ask = Ask {
        size: t.get::<Option<u32>>("size")?.unwrap_or(base.size).clamp(32, 2048),
        most: t.get::<Option<usize>>("most")?.unwrap_or(base.most).clamp(1, 512),
        shortest: t.get::<Option<f32>>("shortest")?.unwrap_or(base.shortest).clamp(0.0, 1.0),
    };
    let on_done: Function = t.get("on_done")?;
    let (id, out) = inbox.listen(on_done);
    std::thread::Builder::new()
        .name("genoma-contours".into())
        .spawn(move || {
            let values = match contours(&source, ask) {
                Some(c) => vec![
                    Dyn::List(
                        c.lines
                            .into_iter()
                            .map(|line| Dyn::List(line.into_iter().flat_map(|[x, y]| [Dyn::Num(x as f64), Dyn::Num(y as f64)]).collect()))
                            .collect(),
                    ),
                    Dyn::Int(c.width as i64),
                    Dyn::Int(c.height as i64),
                ],
                None => {
                    log::warn!("g.contours {source}: could not be read");
                    vec![Dyn::Nil]
                }
            };
            let _ = out.send(Delivery { id, result: Ok(values), last: true });
        })
        .map_err(|e| err(format!("g.contours: no thread: {e}")))?;
    Ok(())
}
