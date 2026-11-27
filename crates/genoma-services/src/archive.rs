//! libarchive, as far as `@genoma/files` uses it to open and make archives
//! (files::Task::Extract and Compress): the library behind pacman, gvfs and
//! GNOME's own archives, so every format a desktop meets — zip, tar with any
//! compression, 7z, rar, iso, cpio, a lone .gz — is read, and zip, tar.*,
//! 7z written. Only what is used is bound; every pointer is owned by a
//! wrapper that frees it.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_void},
    os::unix::ffi::OsStrExt,
    path::Path,
};

#[repr(C)]
pub struct RawArchive {
    _private: [u8; 0],
}

#[repr(C)]
pub struct RawEntry {
    _private: [u8; 0],
}

pub const OK: c_int = 0;
pub const EOF: c_int = 1;
pub const WARN: c_int = -20;
pub const FAILED: c_int = -25;

pub const IFMT: c_uint = 0o170000;
pub const IFREG: c_uint = 0o100000;
pub const IFLNK: c_uint = 0o120000;
pub const IFDIR: c_uint = 0o040000;

const EXTRACT_PERM: c_int = 0x0002;
const EXTRACT_TIME: c_int = 0x0004;
const EXTRACT_NO_OVERWRITE: c_int = 0x0008;
const EXTRACT_SECURE_SYMLINKS: c_int = 0x0100;
const EXTRACT_SECURE_NODOTDOT: c_int = 0x0200;

#[link(name = "archive")]
unsafe extern "C" {
    fn archive_read_new() -> *mut RawArchive;
    fn archive_read_support_filter_all(a: *mut RawArchive) -> c_int;
    fn archive_read_support_format_all(a: *mut RawArchive) -> c_int;
    fn archive_read_support_format_raw(a: *mut RawArchive) -> c_int;
    fn archive_read_add_passphrase(a: *mut RawArchive, p: *const c_char) -> c_int;
    fn archive_read_open_filename(a: *mut RawArchive, name: *const c_char, block: usize) -> c_int;
    fn archive_read_next_header(a: *mut RawArchive, entry: *mut *mut RawEntry) -> c_int;
    fn archive_read_data_block(a: *mut RawArchive, buff: *mut *const c_void, size: *mut usize, offset: *mut i64) -> c_int;
    fn archive_read_has_encrypted_entries(a: *mut RawArchive) -> c_int;
    fn archive_read_free(a: *mut RawArchive) -> c_int;

    fn archive_write_new() -> *mut RawArchive;
    fn archive_write_set_format_filter_by_ext(a: *mut RawArchive, name: *const c_char) -> c_int;
    fn archive_write_set_options(a: *mut RawArchive, opts: *const c_char) -> c_int;
    fn archive_write_set_passphrase(a: *mut RawArchive, p: *const c_char) -> c_int;
    fn archive_write_open_filename(a: *mut RawArchive, name: *const c_char) -> c_int;
    fn archive_write_disk_new() -> *mut RawArchive;
    fn archive_write_disk_set_options(a: *mut RawArchive, flags: c_int) -> c_int;
    fn archive_write_header(a: *mut RawArchive, entry: *mut RawEntry) -> c_int;
    fn archive_write_data(a: *mut RawArchive, buff: *const c_void, size: usize) -> isize;
    fn archive_write_data_block(a: *mut RawArchive, buff: *const c_void, size: usize, offset: i64) -> isize;
    fn archive_write_finish_entry(a: *mut RawArchive) -> c_int;
    fn archive_write_close(a: *mut RawArchive) -> c_int;
    fn archive_write_free(a: *mut RawArchive) -> c_int;

    fn archive_filter_bytes(a: *mut RawArchive, n: c_int) -> i64;
    fn archive_error_string(a: *mut RawArchive) -> *const c_char;

    fn archive_entry_new() -> *mut RawEntry;
    fn archive_entry_free(e: *mut RawEntry);
    fn archive_entry_pathname(e: *mut RawEntry) -> *const c_char;
    fn archive_entry_hardlink(e: *mut RawEntry) -> *const c_char;
    fn archive_entry_filetype(e: *mut RawEntry) -> c_uint;
    fn archive_entry_is_encrypted(e: *mut RawEntry) -> c_int;
    fn archive_entry_copy_pathname(e: *mut RawEntry, name: *const c_char);
    fn archive_entry_copy_hardlink(e: *mut RawEntry, name: *const c_char);
    fn archive_entry_copy_symlink(e: *mut RawEntry, name: *const c_char);
    fn archive_entry_set_filetype(e: *mut RawEntry, kind: c_uint);
    fn archive_entry_set_perm(e: *mut RawEntry, perm: c_uint);
    fn archive_entry_set_size(e: *mut RawEntry, size: i64);
    fn archive_entry_set_mtime(e: *mut RawEntry, sec: i64, nsec: c_long);
}

fn c_path(path: &Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| "a name with a NUL in it".to_owned())
}

fn c_bytes(bytes: &[u8]) -> Result<CString, String> {
    CString::new(bytes).map_err(|_| "a name with a NUL in it".to_owned())
}

fn error_of(a: *mut RawArchive) -> String {
    // SAFETY: `a` is a live archive; the string, when there is one, is its own
    // until its next call, and is copied here.
    let s = unsafe { archive_error_string(a) };
    if s.is_null() {
        "the archive cannot be read".to_owned()
    } else {
        unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
    }
}

/// An entry of an archive being read: valid until the next header.
#[derive(Clone, Copy)]
pub struct Entry(*mut RawEntry);

impl Entry {
    fn text(p: *const c_char) -> Option<Vec<u8>> {
        // SAFETY: libarchive's strings live as long as the entry.
        if p.is_null() { None } else { Some(unsafe { CStr::from_ptr(p) }.to_bytes().to_vec()) }
    }

    pub fn path(&self) -> Option<Vec<u8>> {
        Self::text(unsafe { archive_entry_pathname(self.0) })
    }

    pub fn hardlink(&self) -> Option<Vec<u8>> {
        Self::text(unsafe { archive_entry_hardlink(self.0) })
    }

    pub fn kind(&self) -> c_uint {
        unsafe { archive_entry_filetype(self.0) & IFMT }
    }

    pub fn encrypted(&self) -> bool {
        unsafe { archive_entry_is_encrypted(self.0) > 0 }
    }

    pub fn set_path(&self, path: &[u8]) -> Result<(), String> {
        let c = c_bytes(path)?;
        unsafe { archive_entry_copy_pathname(self.0, c.as_ptr()) };
        Ok(())
    }

    pub fn set_hardlink(&self, path: &[u8]) -> Result<(), String> {
        let c = c_bytes(path)?;
        unsafe { archive_entry_copy_hardlink(self.0, c.as_ptr()) };
        Ok(())
    }
}

/// An archive being read, from its start.
pub struct Reader(*mut RawArchive);

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { archive_read_free(self.0) };
    }
}

impl Reader {
    /// `raw`: what is not an archive but one file compressed (a lone .gz),
    /// read as its single entry.
    pub fn open(path: &Path, passphrase: Option<&str>, raw: bool) -> Result<Reader, String> {
        let a = unsafe { archive_read_new() };
        if a.is_null() {
            return Err("out of memory".into());
        }
        let r = Reader(a);
        unsafe {
            archive_read_support_filter_all(a);
            if raw {
                archive_read_support_format_raw(a);
            } else {
                archive_read_support_format_all(a);
            }
        }
        if let Some(p) = passphrase {
            let c = CString::new(p).map_err(|_| "a password with a NUL in it".to_owned())?;
            unsafe { archive_read_add_passphrase(a, c.as_ptr()) };
        }
        let name = c_path(path)?;
        if unsafe { archive_read_open_filename(a, name.as_ptr(), 64 << 10) } != OK {
            return Err(error_of(a));
        }
        Ok(r)
    }

    /// The next entry, or None at the end.
    pub fn next(&mut self) -> Result<Option<Entry>, String> {
        let mut e: *mut RawEntry = std::ptr::null_mut();
        match unsafe { archive_read_next_header(self.0, &mut e) } {
            EOF => Ok(None),
            OK | WARN => Ok(Some(Entry(e))),
            _ => Err(error_of(self.0)),
        }
    }

    /// The next piece of the entry's data and where it goes, or None at its
    /// end.
    pub fn block(&mut self) -> Result<Option<(&[u8], i64)>, String> {
        let mut buff: *const c_void = std::ptr::null();
        let mut size = 0usize;
        let mut offset = 0i64;
        match unsafe { archive_read_data_block(self.0, &mut buff, &mut size, &mut offset) } {
            EOF => Ok(None),
            OK | WARN => {
                // SAFETY: libarchive's buffer holds `size` bytes until the
                // next read.
                let data = if size == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(buff as *const u8, size) } };
                Ok(Some((data, offset)))
            }
            _ => Err(error_of(self.0)),
        }
    }

    /// The archive's own bytes read so far: how far it is, whatever it holds.
    pub fn consumed(&self) -> u64 {
        unsafe { archive_filter_bytes(self.0, -1) }.max(0) as u64
    }

    /// Whether some of what it holds is locked by a password.
    pub fn encrypted(&self) -> bool {
        unsafe { archive_read_has_encrypted_entries(self.0) > 0 }
    }
}

/// Entries put on the disk, never over what is there, never outside their
/// folder: no "..", no writing through a link. Their names are made
/// absolute by the caller, below the folder they go in, from names made
/// safe first (files.rs's safe_relative).
pub struct Disk(*mut RawArchive);

impl Drop for Disk {
    fn drop(&mut self) {
        unsafe { archive_write_free(self.0) };
    }
}

impl Disk {
    pub fn new() -> Result<Disk, String> {
        let a = unsafe { archive_write_disk_new() };
        if a.is_null() {
            return Err("out of memory".into());
        }
        let flags = EXTRACT_PERM
            | EXTRACT_TIME
            | EXTRACT_NO_OVERWRITE
            | EXTRACT_SECURE_SYMLINKS
            | EXTRACT_SECURE_NODOTDOT;
        unsafe { archive_write_disk_set_options(a, flags) };
        Ok(Disk(a))
    }

    /// FAILED: this entry cannot be put there, the others can.
    pub fn header(&mut self, e: Entry) -> Result<c_int, String> {
        match unsafe { archive_write_header(self.0, e.0) } {
            s @ (OK | WARN | FAILED) => Ok(s),
            _ => Err(error_of(self.0)),
        }
    }

    pub fn write(&mut self, data: &[u8], offset: i64) -> Result<(), String> {
        if unsafe { archive_write_data_block(self.0, data.as_ptr() as *const c_void, data.len(), offset) } < 0 {
            return Err(error_of(self.0));
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), String> {
        match unsafe { archive_write_finish_entry(self.0) } {
            OK | WARN => Ok(()),
            _ => Err(error_of(self.0)),
        }
    }
}

/// An entry being made, for an archive being written.
pub struct NewEntry(*mut RawEntry);

impl Drop for NewEntry {
    fn drop(&mut self) {
        unsafe { archive_entry_free(self.0) };
    }
}

impl NewEntry {
    pub fn new(path: &[u8], kind: c_uint, perm: u32, size: i64, mtime: (i64, i64)) -> Result<NewEntry, String> {
        let e = unsafe { archive_entry_new() };
        if e.is_null() {
            return Err("out of memory".into());
        }
        let made = NewEntry(e);
        let c = c_bytes(path)?;
        unsafe {
            archive_entry_copy_pathname(e, c.as_ptr());
            archive_entry_set_filetype(e, kind);
            archive_entry_set_perm(e, perm & 0o7777);
            archive_entry_set_size(e, size);
            archive_entry_set_mtime(e, mtime.0, mtime.1 as c_long);
        }
        Ok(made)
    }

    pub fn set_symlink(&self, target: &[u8]) -> Result<(), String> {
        let c = c_bytes(target)?;
        unsafe { archive_entry_copy_symlink(self.0, c.as_ptr()) };
        Ok(())
    }
}

/// An archive being written, its format and compression by the name it will
/// have (.zip, .tar.xz, .7z, .tar.gz, .tar.zst, …).
pub struct Writer(*mut RawArchive);

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe { archive_write_free(self.0) };
    }
}

impl Writer {
    /// Written into `out`, as the format `named` says. A password locks a
    /// zip (AES-256); no other format can be locked here.
    pub fn new(named: &Path, out: &Path, passphrase: Option<&str>) -> Result<Writer, String> {
        let a = unsafe { archive_write_new() };
        if a.is_null() {
            return Err("out of memory".into());
        }
        let w = Writer(a);
        let name = c_path(named)?;
        if unsafe { archive_write_set_format_filter_by_ext(a, name.as_ptr()) } != OK {
            return Err(format!("{} is not a kind of archive that can be made", named.display()));
        }
        if let Some(p) = passphrase {
            let zip = named.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"));
            if !zip {
                return Err("only a zip can be locked with a password".into());
            }
            let opts = c"zip:encryption=aes256";
            let pass = CString::new(p).map_err(|_| "a password with a NUL in it".to_owned())?;
            if unsafe { archive_write_set_options(a, opts.as_ptr()) } != OK
                || unsafe { archive_write_set_passphrase(a, pass.as_ptr()) } != OK
            {
                return Err(error_of(a));
            }
        }
        let out = c_path(out)?;
        if unsafe { archive_write_open_filename(a, out.as_ptr()) } != OK {
            return Err(error_of(a));
        }
        Ok(w)
    }

    pub fn header(&mut self, e: &NewEntry) -> Result<(), String> {
        match unsafe { archive_write_header(self.0, e.0) } {
            OK | WARN => Ok(()),
            _ => Err(error_of(self.0)),
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let mut at = 0;
        while at < data.len() {
            let n = unsafe { archive_write_data(self.0, data[at..].as_ptr() as *const c_void, data.len() - at) };
            if n <= 0 {
                return Err(error_of(self.0));
            }
            at += n as usize;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), String> {
        match unsafe { archive_write_finish_entry(self.0) } {
            OK | WARN => Ok(()),
            _ => Err(error_of(self.0)),
        }
    }

    /// Everything written out: until this, the archive is not whole.
    pub fn close(&mut self) -> Result<(), String> {
        match unsafe { archive_write_close(self.0) } {
            OK | WARN => Ok(()),
            _ => Err(error_of(self.0)),
        }
    }
}

/// What an archive's contents are called once out of it: its name without
/// what says it is an archive ("photos.tar.gz" → "photos").
pub fn stem(name: &[u8]) -> Vec<u8> {
    const DOUBLE: [&[u8]; 6] = [b".tar.gz", b".tar.xz", b".tar.zst", b".tar.bz2", b".tar.lz", b".tar.lzma"];
    let lower = name.to_ascii_lowercase();
    for ending in DOUBLE {
        if lower.ends_with(ending) && lower.len() > ending.len() {
            return name[..name.len() - ending.len()].to_vec();
        }
    }
    match name.iter().rposition(|b| *b == b'.') {
        Some(dot) if dot > 0 => name[..dot].to_vec(),
        _ => name.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archives_contents_are_named_without_its_endings() {
        assert_eq!(stem(b"photos.tar.gz"), b"photos");
        assert_eq!(stem(b"Photos.TAR.XZ"), b"Photos");
        assert_eq!(stem(b"report.zip"), b"report");
        assert_eq!(stem(b"notes.txt.gz"), b"notes.txt");
        assert_eq!(stem(b".hidden"), b".hidden");
        assert_eq!(stem(b"plain"), b"plain");
    }
}
