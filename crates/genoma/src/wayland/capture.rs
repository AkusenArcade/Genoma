//! `@genoma/capture`: screenshots through `wlr-screencopy` on the main
//! thread's connection, bound once an interface requires the service.
//!
//! A picture is one frame from each output it touches, each copied into a
//! shared-memory buffer. Once every frame is in, the pictures are turned
//! upright, cut to the area, brought to one scale and joined on a thread of
//! its own (`genoma-capture`), which also writes the PNG: a 4K picture
//! takes longer to encode than a frame lasts. The answer comes back to the
//! loop through a channel.

use std::{fs::File, os::unix::fs::FileExt, rc::Rc};

use genoma_services::capture::{Area, Capture, Keep, Of, Taken};
use image::{
    RgbaImage,
    imageops::{self, FilterType},
};
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    reexports::{
        calloop::channel::{self, Sender},
        client::{
            Connection, Proxy, QueueHandle, WEnum,
            protocol::{
                wl_buffer::WlBuffer,
                wl_output::{Transform, WlOutput},
                wl_shm::{Format, WlShm},
            },
        },
        protocols_wlr::screencopy::v1::client::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        },
    },
};

use super::{App, Ignore};

pub(super) struct Cm {
    service: Rc<Capture>,
    manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<WlShm>,
    jobs: Vec<Job>,
    post: Sender<(u64, Taken)>,
}

struct Job {
    id: u64,
    area: Area,
    keep: Keep,
    parts: Vec<Part>,
}

struct Part {
    frame: ZwlrScreencopyFrameV1,
    logical: Area,
    transform: Transform,
    /// The shared-memory format the compositor offers: format, width,
    /// height, stride.
    offer: Option<(Format, u32, u32, u32)>,
    buffer: Option<(WlBuffer, File)>,
    y_invert: bool,
    raw: Option<Raw>,
}

/// A frame's user data: which picture, which of its outputs.
pub(super) struct FrameData {
    job: u64,
    part: usize,
}

/// One output's frame as it came, with where it sits.
pub(crate) struct Raw {
    pub logical: Area,
    pub transform: Transform,
    pub y_invert: bool,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bytes: Vec<u8>,
}

/// Whether the frame can be read in `format`: the 8-bit and 10-bit RGB
/// layouts compositors offer for screencopy.
fn readable(format: Format) -> bool {
    matches!(
        format,
        Format::Argb8888
            | Format::Xrgb8888
            | Format::Abgr8888
            | Format::Xbgr8888
            | Format::Argb2101010
            | Format::Xrgb2101010
            | Format::Abgr2101010
            | Format::Xbgr2101010
            | Format::Rgb888
            | Format::Bgr888
    )
}

/// The frame as RGBA in the buffer's own orientation, opaque: an output has
/// no transparency, whatever its alpha channel holds.
fn rgba(raw: &Raw) -> Result<RgbaImage, String> {
    let (w, h, stride) = (raw.width as usize, raw.height as usize, raw.stride as usize);
    let bpp = if matches!(raw.format, Format::Rgb888 | Format::Bgr888) { 3 } else { 4 };
    if raw.bytes.len() < stride * h.saturating_sub(1) + w * bpp {
        return Err("the frame is shorter than its size".into());
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let line = &raw.bytes[row * stride..row * stride + w * bpp];
        for px in line.chunks_exact(bpp) {
            let ten = |v: u32| ((v & 0x3ff) >> 2) as u8;
            let rgb = match raw.format {
                // Little-endian words: the bytes run B, G, R, A.
                Format::Argb8888 | Format::Xrgb8888 => [px[2], px[1], px[0]],
                Format::Abgr8888 | Format::Xbgr8888 => [px[0], px[1], px[2]],
                Format::Argb2101010 | Format::Xrgb2101010 => {
                    let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                    [ten(v >> 20), ten(v >> 10), ten(v)]
                }
                Format::Abgr2101010 | Format::Xbgr2101010 => {
                    let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                    [ten(v), ten(v >> 10), ten(v >> 20)]
                }
                Format::Rgb888 => [px[2], px[1], px[0]],
                Format::Bgr888 => [px[0], px[1], px[2]],
                _ => return Err(format!("unreadable format {:?}", raw.format)),
            };
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let image = RgbaImage::from_raw(raw.width, raw.height, out).ok_or("the frame does not fit its size")?;
    Ok(if raw.y_invert { imageops::flip_vertical(&image) } else { image })
}

/// The frame as the output shows it in the layout. The buffer holds what
/// goes to the panel, the content turned by the output's transform
/// (counter-clockwise, after any flip): undone here.
fn upright(image: RgbaImage, transform: Transform) -> RgbaImage {
    match transform {
        Transform::_90 => imageops::rotate90(&image),
        Transform::_180 => imageops::rotate180(&image),
        Transform::_270 => imageops::rotate270(&image),
        Transform::Flipped => imageops::flip_horizontal(&image),
        Transform::Flipped90 => imageops::flip_horizontal(&imageops::rotate90(&image)),
        Transform::Flipped180 => imageops::flip_horizontal(&imageops::rotate180(&image)),
        Transform::Flipped270 => imageops::flip_horizontal(&imageops::rotate270(&image)),
        _ => image,
    }
}

/// The frames joined into the picture of `area`, at the finest scale among
/// the outputs it touches; what no output covers stays transparent.
pub(crate) fn compose(raws: &[Raw], area: Area) -> Result<RgbaImage, String> {
    let mut pictures = Vec::with_capacity(raws.len());
    for raw in raws {
        pictures.push((raw.logical, upright(rgba(raw)?, raw.transform)));
    }
    let scale = pictures.iter().map(|(l, p)| p.width() as f64 / l.w.max(1.0)).fold(0.0, f64::max).max(f64::MIN_POSITIVE);
    let size = |v: f64| (v * scale).round().max(1.0) as u32;
    let mut canvas = RgbaImage::new(size(area.w), size(area.h));
    for (logical, picture) in &pictures {
        let Some(part) = logical.intersection(&area) else { continue };
        let own = picture.width() as f64 / logical.w.max(1.0);
        let x = (((part.x - logical.x) * own).round() as u32).min(picture.width() - 1);
        let y = (((part.y - logical.y) * own).round() as u32).min(picture.height() - 1);
        let w = ((part.w * own).round().max(1.0) as u32).min(picture.width() - x);
        let h = ((part.h * own).round().max(1.0) as u32).min(picture.height() - y);
        let mut piece = imageops::crop_imm(picture, x, y, w, h).to_image();
        let (dw, dh) = (size(part.w), size(part.h));
        if (w, h) != (dw, dh) {
            piece = imageops::resize(&piece, dw, dh, FilterType::Triangle);
        }
        imageops::overlay(&mut canvas, &piece, ((part.x - area.x) * scale).round() as i64, ((part.y - area.y) * scale).round() as i64);
    }
    Ok(canvas)
}

/// What a finished picture becomes.
fn keep(raws: &[Raw], area: Area, keep: &Keep) -> Taken {
    let picture = compose(raws, area)?;
    match keep {
        Keep::File(path) => {
            if let Some(folder) = path.parent().filter(|f| !f.as_os_str().is_empty()) {
                std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
            }
            picture.save_with_format(path, image::ImageFormat::Png).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(path.to_string_lossy().into_owned())
        }
        Keep::Colour => {
            let [r, g, b, a] = picture.get_pixel(0, 0).0;
            if a == 0 {
                return Err("no output there".into());
            }
            Ok(format!("#{r:02x}{g:02x}{b:02x}"))
        }
    }
}

fn logical(info: &smithay_client_toolkit::output::OutputInfo) -> Option<Area> {
    let (x, y) = info.logical_position?;
    let (w, h) = info.logical_size?;
    Some(Area { x: x as f64, y: y as f64, w: w as f64, h: h as f64 })
}

impl App {
    /// Binds `wlr-screencopy` and `wl_shm` for `service`, once.
    pub fn start_capture(&mut self, service: Rc<Capture>) {
        if self.cm.is_some() {
            return;
        }
        let manager = self.globals.bind::<ZwlrScreencopyManagerV1, _, _>(&self.qh, 1..=3, Ignore).ok();
        let shm = self.globals.bind::<WlShm, _, _>(&self.qh, 1..=1, Ignore).ok();
        service.available.set(manager.is_some() && shm.is_some());
        let (post, answers) = channel::channel::<(u64, Taken)>();
        let answering = service.clone();
        if let Some(handle) = &self.handle {
            let _ = handle.insert_source(answers, move |event, _, _| {
                if let channel::Event::Msg((id, taken)) = event {
                    answering.finish(id, taken);
                }
            });
        }
        self.cm = Some(Cm { service, manager, shm, jobs: Vec::new(), post });
    }

    /// The pictures asked for since the last turn, begun.
    pub fn sync_capture(&mut self) {
        let Some(cm) = self.cm.as_mut() else { return };
        let requests = cm.service.take_requests();
        if requests.is_empty() {
            return;
        }
        let outputs: Vec<(WlOutput, String, Area, Transform)> = self
            .outputs
            .outputs()
            .filter_map(|o| {
                let info = self.outputs.info(&o)?;
                Some((o, info.name.clone()?, logical(&info)?, info.transform))
            })
            .collect();
        for (id, shot) in requests {
            let Some(manager) = &cm.manager else {
                let _ = cm.post.send((id, Err("the compositor offers no wlr-screencopy".into())));
                continue;
            };
            let area = match &shot.of {
                Of::Output(name) => match outputs.iter().find(|(_, n, _, _)| n == name) {
                    Some((_, _, a, _)) => *a,
                    None => {
                        let _ = cm.post.send((id, Err(format!("no output {name}"))));
                        continue;
                    }
                },
                Of::Area(a) => *a,
                Of::Everything => match outputs.iter().map(|(_, _, a, _)| *a).reduce(|a, b| a.union(&b)) {
                    Some(a) => a,
                    None => {
                        let _ = cm.post.send((id, Err("no outputs".into())));
                        continue;
                    }
                },
            };
            let parts: Vec<Part> = outputs
                .iter()
                .filter(|(_, _, a, _)| a.intersection(&area).is_some())
                .enumerate()
                .map(|(i, (output, _, a, transform))| Part {
                    frame: manager.capture_output(shot.cursor as i32, output, &self.qh, FrameData { job: id, part: i }),
                    logical: *a,
                    transform: *transform,
                    offer: None,
                    buffer: None,
                    y_invert: false,
                    raw: None,
                })
                .collect();
            if parts.is_empty() {
                let _ = cm.post.send((id, Err("no output there".into())));
                continue;
            }
            cm.jobs.push(Job { id, area, keep: shot.keep, parts });
        }
        let _ = self.conn.flush();
    }
}

impl Cm {
    /// Every frame of job `id` let go, and the job with them.
    fn fail(&mut self, id: u64, why: String) {
        let Some(at) = self.jobs.iter().position(|j| j.id == id) else { return };
        let job = self.jobs.remove(at);
        for part in job.parts {
            if let Some((buffer, _)) = part.buffer {
                buffer.destroy();
            }
            part.frame.destroy();
        }
        let _ = self.post.send((id, Err(why)));
    }

    /// The buffer for a frame whose format is known, and the copy asked.
    fn copy(&mut self, id: u64, part: usize, qh: &QueueHandle<App>) {
        let Some(shm) = self.shm.clone() else { return };
        let Some(p) = self.jobs.iter_mut().find(|j| j.id == id).and_then(|j| j.parts.get_mut(part)) else { return };
        if p.buffer.is_some() {
            return;
        }
        let Some((format, width, height, stride)) = p.offer else {
            self.fail(id, "the compositor offers no format Genoma reads".into());
            return;
        };
        let size = stride as usize * height as usize;
        let file = match rustix::fs::memfd_create("genoma-capture", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(std::io::Error::from)
            .map(File::from)
            .and_then(|f| f.set_len(size as u64).map(|()| f))
        {
            Ok(f) => f,
            Err(e) => {
                self.fail(id, format!("shared memory: {e}"));
                return;
            }
        };
        let pool = shm.create_pool(std::os::fd::AsFd::as_fd(&file), size as i32, qh, Ignore);
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, Ignore);
        pool.destroy();
        p.frame.copy(&buffer);
        p.buffer = Some((buffer, file));
    }

    /// A frame copied: once all of a job's are, the picture is made apart.
    fn ready(&mut self, id: u64, part: usize) {
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) else { return };
        let Some(p) = job.parts.get_mut(part) else { return };
        let (Some((buffer, file)), Some((format, width, height, stride))) = (p.buffer.take(), p.offer) else { return };
        let mut bytes = vec![0; stride as usize * height as usize];
        let read = file.read_exact_at(&mut bytes, 0);
        buffer.destroy();
        p.frame.destroy();
        if let Err(e) = read {
            self.fail(id, format!("reading the frame: {e}"));
            return;
        }
        p.raw = Some(Raw { logical: p.logical, transform: p.transform, y_invert: p.y_invert, format, width, height, stride, bytes });
        if job.parts.iter().any(|p| p.raw.is_none()) {
            return;
        }
        let at = self.jobs.iter().position(|j| j.id == id).expect("the job just found");
        let job = self.jobs.remove(at);
        let raws: Vec<Raw> = job.parts.into_iter().filter_map(|p| p.raw).collect();
        let (area, how, post) = (job.area, job.keep, self.post.clone());
        let spawned = std::thread::Builder::new().name("genoma-capture".into()).spawn(move || {
            let _ = post.send((id, keep(&raws, area, &how)));
        });
        if let Err(e) = spawned {
            let _ = self.post.send((id, Err(format!("no thread for the picture: {e}"))));
        }
    }
}

impl Dispatch2<ZwlrScreencopyFrameV1, App> for FrameData {
    fn event(&self, app: &mut App, frame: &ZwlrScreencopyFrameV1, event: zwlr_screencopy_frame_v1::Event, _: &Connection, qh: &QueueHandle<App>) {
        let Some(cm) = app.cm.as_mut() else { return };
        let (id, part) = (self.job, self.part);
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format: WEnum::Value(format), width, height, stride } => {
                if let Some(p) = cm.jobs.iter_mut().find(|j| j.id == id).and_then(|j| j.parts.get_mut(part))
                    && p.offer.is_none()
                    && readable(format)
                {
                    p.offer = Some((format, width, height, stride));
                }
                // Before version 3 the offer is all there is: copy now.
                if frame.version() < 3 {
                    cm.copy(id, part, qh);
                }
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => cm.copy(id, part, qh),
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                if let Some(p) = cm.jobs.iter_mut().find(|j| j.id == id).and_then(|j| j.parts.get_mut(part)) {
                    p.y_invert = matches!(flags, WEnum::Value(f) if f.contains(zwlr_screencopy_frame_v1::Flags::YInvert));
                }
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => cm.ready(id, part),
            zwlr_screencopy_frame_v1::Event::Failed => cm.fail(id, "the compositor would not copy the screen".into()),
            _ => {}
        }
        let _ = app.conn.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of `w`×`h` in XRGB8888, each pixel's red its x and green its
    /// y.
    fn frame(logical: Area, w: u32, h: u32, transform: Transform) -> Raw {
        let mut bytes = Vec::new();
        for y in 0..h {
            for x in 0..w {
                bytes.extend_from_slice(&[0, y as u8, x as u8, 0]);
            }
        }
        Raw { logical, transform, y_invert: false, format: Format::Xrgb8888, width: w, height: h, stride: w * 4, bytes }
    }

    #[test]
    fn formats() {
        let mut raw = frame(Area { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, 1, 1, Transform::Normal);
        raw.bytes = vec![0x30, 0x20, 0x10, 0x00];
        assert_eq!(rgba(&raw).unwrap().get_pixel(0, 0).0, [0x10, 0x20, 0x30, 255]);
        raw.format = Format::Xbgr8888;
        assert_eq!(rgba(&raw).unwrap().get_pixel(0, 0).0, [0x30, 0x20, 0x10, 255]);
        // 10 bits a channel: red all on, green half, blue off.
        raw.format = Format::Xrgb2101010;
        raw.bytes = ((0x3ffu32 << 20) | (0x200 << 10)).to_le_bytes().to_vec();
        assert_eq!(rgba(&raw).unwrap().get_pixel(0, 0).0, [255, 128, 0, 255]);
    }

    #[test]
    fn stride_and_flip() {
        let mut raw = frame(Area { x: 0.0, y: 0.0, w: 2.0, h: 2.0 }, 2, 2, Transform::Normal);
        // Rows padded to 12 bytes.
        let rows: Vec<u8> = raw.bytes.chunks(8).flat_map(|r| r.iter().copied().chain([9, 9, 9, 9])).collect();
        raw.bytes = rows;
        raw.stride = 12;
        raw.y_invert = true;
        let image = rgba(&raw).unwrap();
        assert_eq!(image.get_pixel(1, 0).0, [1, 1, 0, 255], "the bottom row comes first");
    }

    #[test]
    fn area_across_outputs() {
        // His layout: an ultrawide above, a 1080p below and to the right.
        let top = frame(Area { x: 0.0, y: 0.0, w: 40.0, h: 20.0 }, 40, 20, Transform::Normal);
        let bottom = frame(Area { x: 10.0, y: 20.0, w: 20.0, h: 10.0 }, 20, 10, Transform::Normal);
        let area = Area { x: 5.0, y: 15.0, w: 10.0, h: 10.0 };
        let picture = compose(&[top, bottom], area).unwrap();
        assert_eq!(picture.dimensions(), (10, 10));
        assert_eq!(picture.get_pixel(0, 0).0, [5, 15, 0, 255], "from the top output");
        assert_eq!(picture.get_pixel(9, 9).0, [4, 4, 0, 255], "from the bottom one, its own pixel (4, 4)");
        assert_eq!(picture.get_pixel(0, 9).0[3], 0, "no output covers it");
    }

    #[test]
    fn finest_scale_wins() {
        let sharp = frame(Area { x: 0.0, y: 0.0, w: 10.0, h: 10.0 }, 20, 20, Transform::Normal);
        let plain = frame(Area { x: 10.0, y: 0.0, w: 10.0, h: 10.0 }, 10, 10, Transform::Normal);
        let picture = compose(&[sharp, plain], Area { x: 0.0, y: 0.0, w: 20.0, h: 10.0 }).unwrap();
        assert_eq!(picture.dimensions(), (40, 20));
    }

    #[test]
    fn turned_upright() {
        // A 90° output: its 3×2 panel buffer shows a 2×3 logical area.
        let raw = frame(Area { x: 0.0, y: 0.0, w: 2.0, h: 3.0 }, 3, 2, Transform::_90);
        let picture = compose(&[raw], Area { x: 0.0, y: 0.0, w: 2.0, h: 3.0 }).unwrap();
        assert_eq!(picture.dimensions(), (2, 3));
    }
}
