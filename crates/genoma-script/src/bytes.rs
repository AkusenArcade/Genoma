//! Bytes for protocols written in Luau: a SHA-256 digest, base64 both ways
//! (the standard alphabet with padding, or the URL one without, as OAuth's
//! PKCE wants it), and random bytes from the kernel. Luau strings carry
//! bytes, so a digest is a string of 32 bytes, not hex.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use mlua::{Lua, LuaString, Table};
use sha2::{Digest, Sha256};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// The most `g.random_bytes` gives at once: a key, a nonce, a verifier.
const MOST_RANDOM: usize = 4096;

fn url(options: Option<Table>) -> mlua::Result<bool> {
    Ok(match options {
        Some(t) => t.get::<Option<bool>>("url")?.unwrap_or(false),
        None => false,
    })
}

/// `g.sha256(data)`: the digest, 32 bytes.
pub fn sha256(lua: &Lua, data: LuaString) -> mlua::Result<LuaString> {
    lua.create_string(Sha256::digest(&*data.as_bytes()))
}

/// `g.base64.encode(data, { url = true })`.
pub fn encode(_: &Lua, (data, options): (LuaString, Option<Table>)) -> mlua::Result<String> {
    let bytes = data.as_bytes();
    Ok(if url(options)? { URL_SAFE_NO_PAD.encode(&*bytes) } else { STANDARD.encode(&*bytes) })
}

/// `g.base64.decode(text, { url = true })`: the bytes, or nil when the
/// text is not base64 of that alphabet.
pub fn decode(lua: &Lua, (text, options): (LuaString, Option<Table>)) -> mlua::Result<Option<LuaString>> {
    let text = text.as_bytes();
    let decoded = if url(options)? { URL_SAFE_NO_PAD.decode(&*text) } else { STANDARD.decode(&*text) };
    match decoded {
        Ok(bytes) => Ok(Some(lua.create_string(bytes)?)),
        Err(_) => Ok(None),
    }
}

/// `g.random_bytes(n)`: n bytes from the kernel's generator, fit for keys
/// and verifiers (Luau's math.random is not).
pub fn random_bytes(lua: &Lua, n: usize) -> mlua::Result<LuaString> {
    if n > MOST_RANDOM {
        return Err(err(format!("g.random_bytes: at most {MOST_RANDOM} at once")));
    }
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        match rustix::rand::getrandom(&mut buf[filled..], rustix::rand::GetRandomFlags::empty()) {
            Ok(k) => filled += k,
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(err(format!("g.random_bytes: {e}"))),
        }
    }
    lua.create_string(buf)
}

#[cfg(test)]
mod tests {
    use mlua::Lua;

    #[test]
    fn digests_and_base64_as_the_standards_say() {
        let lua = Lua::new();
        let g = lua.create_table().unwrap();
        g.set("sha256", lua.create_function(super::sha256).unwrap()).unwrap();
        let b64 = lua.create_table().unwrap();
        b64.set("encode", lua.create_function(super::encode).unwrap()).unwrap();
        b64.set("decode", lua.create_function(super::decode).unwrap()).unwrap();
        g.set("base64", b64).unwrap();
        g.set("random_bytes", lua.create_function(super::random_bytes).unwrap()).unwrap();
        lua.globals().set("g", g).unwrap();
        let checks: Vec<String> = lua
            .load(
                r#"
                local abc = g.sha256("abc")
                local hex = string.gsub(abc, ".", function(c) return string.format("%02x", string.byte(c)) end)
                -- RFC 7636, appendix B: the verifier and its S256 challenge.
                local challenge = g.base64.encode(g.sha256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), { url = true })
                return {
                    hex,
                    g.base64.encode("hello world"),
                    g.base64.decode("aGVsbG8gd29ybGQ="),
                    tostring(g.base64.decode("not base64!")),
                    challenge,
                    tostring(#g.random_bytes(32)),
                    tostring(g.random_bytes(16) ~= g.random_bytes(16)),
                }
                "#,
            )
            .eval()
            .unwrap();
        assert_eq!(checks[0], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(checks[1], "aGVsbG8gd29ybGQ=");
        assert_eq!(checks[2], "hello world");
        assert_eq!(checks[3], "nil");
        assert_eq!(checks[4], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(checks[5], "32");
        assert_eq!(checks[6], "true");
    }
}
