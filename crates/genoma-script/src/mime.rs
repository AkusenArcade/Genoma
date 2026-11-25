//! `@genoma/mime` in Luau.

use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::PathBuf, rc::Rc};

use genoma_services::{apps::Apps, mime};
use mlua::{LuaString, UserData, UserDataMethods, Value as LuaValue};

use crate::{app_table, err};

/// A path as Luau has it, byte for byte (a name need not be UTF-8), with
/// `~/` and `$NAME` expanded.
pub(crate) fn path_of(path: &LuaString) -> PathBuf {
    match path.to_str() {
        Ok(text) => crate::io::expand(&text),
        Err(_) => PathBuf::from(OsStr::from_bytes(&path.as_bytes())),
    }
}

pub(crate) struct MimeRef(pub Rc<Apps>);

impl UserData for MimeRef {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // mime:of(path) → "image/png": by its name, its beginning read only
        // when the name leaves it open.
        methods.add_method("of", |_, _, path: LuaString| Ok(mime::database().of(&path_of(&path)).to_string()));
        // mime:of_name("a.png") → its type, or nil when the name does not
        // say; of several, the likeliest.
        methods.add_method("of_name", |_, _, name: String| {
            Ok(match mime::database().by_name(&name) {
                mime::Named::One(m) => Some(m.to_string()),
                mime::Named::Several(types) => types.first().map(|t| t.to_string()),
                mime::Named::Nothing => None,
            })
        });
        // mime:description("image/png") → "PNG image", in the user's
        // language.
        methods.add_method("description", |_, _, mime: String| Ok(mime::description(&mime)));
        // mime:icon_names("image/png") → { "image-png", "image-x-generic" }.
        methods.add_method("icon_names", |_, _, mime: String| Ok(mime::database().icons(&mime)));
        // mime:icon("image/png", size?) → the first of its icons the theme
        // has, a file (the one drawn for the size), or nil.
        methods.add_method("icon", |_, _, (mime, size): (String, Option<f64>)| {
            let names = mime::database().icons(&mime);
            Ok(names.iter().find_map(|n| crate::themed(n, size)).map(|p| p.display().to_string()))
        });
        // mime:is_a("text/x-csrc", "text/plain") → true.
        methods.add_method("is_a", |_, _, (mime, parent): (String, String)| Ok(mime::database().is_a(&mime, &parent)));
        // mime:apps("image/png") → what opens it, the default first.
        methods.add_method("apps", |lua, this, mime: String| {
            let list = lua.create_table()?;
            for (i, a) in mime::apps(&this.0, &mime).into_iter().enumerate() {
                list.set(i + 1, app_table(lua, a)?)?;
            }
            Ok(list)
        });
        // mime:default("image/png") → what opens it when nobody chooses.
        methods.add_method("default", |lua, this, mime: String| match mime::apps(&this.0, &mime).into_iter().next() {
            Some(a) => Ok(LuaValue::Table(app_table(lua, a)?)),
            None => Ok(LuaValue::Nil),
        });
        // mime:set_default("image/png", "org.gnome.Loupe"): in the user's
        // mimeapps.list, for every program.
        methods.add_method("set_default", |_, this, (mime, id): (String, String)| {
            mime::set_default(&this.0, &mime, &id).map_err(err)
        });
    }
}
