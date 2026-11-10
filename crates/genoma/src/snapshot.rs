//! `genoma snapshot <dir>`: an interface run without a compositor, each of
//! its surfaces drawn into a picture and its tree written as JSON. A shell
//! seen and checked by a script or in CI, on a machine with a GPU but no
//! desktop, and a change compared with the picture before it.
//!
//!   genoma snapshot <dir> [--out DIR] [--size WxH] [--output NAME] [--wait SECONDS]
//!                         [--entry FILE] [--mock @genoma/NAME=FILE]…
//!                         [--ipc "TARGET FUNCTION ARGS…" | --click ID | --sleep SECONDS]…
//!
//! The interface gets one screen of that size and name (1920×1080, "SNAP"
//! unless told), its services as they are on this machine, and `--wait`
//! seconds (1 by default) for its timers and animations. After the load, in
//! the order written, each `--ipc` is a call as `genoma ipc` makes it
//! (`--ipc "cell toggle clock"`), each `--click` a press and release on the
//! node of that id, each `--sleep` time going by. A `--mock` gives a module
//! of the test's own for a service (`--mock @genoma/vitals=busy.luau`, a
//! table with the readings a cell is to be seen with). Into `--out`
//! (`snapshot/` by default) go `<n>-<namespace>.png` for each surface
//! present and `tree.json`.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use genoma_render::{Renderer, cosmic_text::FontSystem};
use genoma_scene::Scene;
use genoma_script::{OutputInfo, Script, SurfaceSpec};
use smithay_client_toolkit::reexports::calloop::EventLoop;

#[derive(Debug)]
struct Options {
    dir: PathBuf,
    out: PathBuf,
    size: (u32, u32),
    output: String,
    wait: f64,
    entry: Option<String>,
    mocks: Vec<(String, PathBuf)>,
    steps: Vec<Step>,
}

/// What is done after the load, in the order written.
#[derive(Debug, PartialEq)]
enum Step {
    Ipc(Vec<String>),
    Click(String),
    Sleep(f64),
}

fn options(args: Vec<String>) -> Result<Options, String> {
    let mut it = args.into_iter();
    let mut o = Options {
        dir: PathBuf::new(),
        out: PathBuf::from("snapshot"),
        size: (1920, 1080),
        output: "SNAP".into(),
        wait: 1.0,
        entry: None,
        mocks: Vec::new(),
        steps: Vec::new(),
    };
    let mut dir = None;
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--out" => o.out = PathBuf::from(value("--out")?),
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once(['x', '×']).ok_or("--size is WIDTHxHEIGHT")?;
                o.size = (w.parse().map_err(|_| "--size: a width")?, h.parse().map_err(|_| "--size: a height")?);
            }
            "--output" => o.output = value("--output")?,
            "--wait" => o.wait = value("--wait")?.parse().map_err(|_| "--wait: seconds")?,
            "--entry" => o.entry = Some(value("--entry")?),
            "--ipc" => o.steps.push(Step::Ipc(value("--ipc")?.split_whitespace().map(str::to_owned).collect())),
            "--click" => o.steps.push(Step::Click(value("--click")?)),
            "--sleep" => o.steps.push(Step::Sleep(value("--sleep")?.parse().map_err(|_| "--sleep: seconds")?)),
            "--mock" => {
                let v = value("--mock")?;
                let (name, path) = v.split_once('=').ok_or("--mock is @genoma/NAME=FILE")?;
                o.mocks.push((name.to_owned(), PathBuf::from(path)));
            }
            other if dir.is_none() && !other.starts_with("--") => dir = Some(PathBuf::from(other)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    o.dir = dir.ok_or("genoma snapshot <dir>: the interface to draw")?;
    Ok(o)
}

/// Whether a surface description is drawn on the one screen.
fn on_screen(spec: &SurfaceSpec, output: &str) -> bool {
    match (&spec.outputs, &spec.output) {
        _ if spec.all_outputs => true,
        (Some(list), _) => list.iter().any(|o| o == output),
        (None, Some(o)) => o == output,
        (None, None) => true,
    }
}

/// The size a surface is laid out at, as the compositor would give it: a
/// window its own, a full or lock surface the screen's, one that spans an
/// edge the screen's width, anything else what its content asks for.
fn bounds(spec: &SurfaceSpec, screen: (u32, u32)) -> (Option<f32>, Option<f32>) {
    let (w, h) = (screen.0 as f32, screen.1 as f32);
    if let Some(window) = &spec.window {
        return (Some(window.size.0 as f32), Some(window.size.1 as f32));
    }
    let has = |edge: &str| spec.anchor.iter().any(|a| a == edge);
    if spec.full || spec.layer == "lock" {
        return (Some(w), Some(h));
    }
    (
        (has("left") && has("right")).then_some(w),
        (has("top") && has("bottom")).then_some(h),
    )
}

pub fn run(args: Vec<String>) -> i32 {
    match snapshot(args) {
        Ok(n) => {
            println!("{n} surfaces drawn");
            0
        }
        Err(e) => {
            eprintln!("genoma snapshot: {e}");
            1
        }
    }
}

fn snapshot(args: Vec<String>) -> Result<usize, String> {
    let o = options(args)?;
    let dir = o.dir.canonicalize().map_err(|e| format!("{}: {e}", o.dir.display()))?;
    std::fs::create_dir_all(&o.out).map_err(|e| format!("{}: {e}", o.out.display()))?;
    let fonts = Arc::new(Mutex::new(FontSystem::new()));
    let renderer = Renderer::spawn(None, fonts.clone()).map_err(|e| e.to_string())?;
    let mut event_loop: EventLoop<'static, ()> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();

    let (services, streams) = crate::services();
    let host: genoma_script::io::HostRef = Rc::new(crate::LoopHost(handle.clone(), fonts.clone()));
    let script = Rc::new(RefCell::new(Script::new(&dir, services.clone(), host)));
    if let Some(entry) = &o.entry {
        script.borrow_mut().set_entry(entry);
    }
    for (name, path) in &o.mocks {
        let path = path.canonicalize().map_err(|e| format!("--mock {name}: {}: {e}", path.display()))?;
        script.borrow_mut().mock(name, path);
    }
    script.borrow_mut().set_outputs(vec![OutputInfo {
        name: o.output.clone(),
        description: "genoma snapshot".into(),
        make: String::new(),
        model: String::new(),
        size: Some(o.size),
    }]);
    let specs = script.borrow_mut().evaluate()?;
    let mut targets = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        if !on_screen(spec, &o.output) {
            continue;
        }
        let area = spec.window.as_ref().map(|w| w.size).or(Some(o.size));
        targets.push((i, Some(o.output.clone()), Rc::new(RefCell::new(Scene::new(fonts.clone()))), area));
    }
    script.borrow_mut().mount(&targets)?;
    let mut sources = crate::Sources { services, streams, bus: None, started: Vec::new() };
    sources.start(|s| script.borrow().wants(s), &handle);
    // The interface's own time: timers, services and animations, with every
    // scene advanced as frames would advance it.
    let mut pass = |seconds: f64| -> Result<(), String> {
        let started = Instant::now();
        loop {
            event_loop.dispatch(Some(Duration::from_millis(16)), &mut ()).map_err(|e| e.to_string())?;
            for (spec, _, scene, _) in &targets {
                let (w, h) = bounds(&specs[*spec], o.size);
                let mut s = scene.borrow_mut();
                let _ = s.advance(Instant::now(), 1.0);
                s.layout(w, h);
                let _ = s.draw_list(1.0);
            }
            if started.elapsed().as_secs_f64() >= seconds {
                return Ok(());
            }
        }
    };
    // Laid out once before anything is pressed: a node is found where it
    // was drawn.
    pass(0.05)?;
    for step in &o.steps {
        match step {
            Step::Ipc(call) => {
                let [target, function, rest @ ..] = call.as_slice() else { return Err(format!("--ipc {call:?}: a target and a function")) };
                let said = script.borrow().ipc(target, function, rest)?;
                log::info!("ipc {target} {function}: {said}");
            }
            Step::Click(id) => {
                let found = targets.iter().enumerate().find_map(|(t, (_, _, scene, _))| scene.borrow().find(id).map(|n| (t, n)));
                let (t, node) = found.ok_or_else(|| format!("--click {id}: no node with that id is shown"))?;
                let [_, _, w, h] = targets[t].2.borrow().extent(node).unwrap_or([0.0; 4]);
                let (x, y) = (w / 2.0, h / 2.0);
                for input in [genoma_script::Input::Press(0x110, x, y), genoma_script::Input::Release(0x110, x, y), genoma_script::Input::Click(0x110, x, y)] {
                    script.borrow().dispatch(t, node, input);
                }
            }
            Step::Sleep(seconds) => pass(*seconds)?,
        }
        pass(0.05)?;
    }
    pass(o.wait)?;

    let wishes = script.borrow().take_wishes().unwrap_or_default();
    let mut tree = Vec::new();
    let mut drawn = 0;
    for (n, (spec_index, _, scene, _)) in targets.iter().enumerate() {
        let spec = &specs[*spec_index];
        let present = wishes.get(n).is_none_or(|w| w.present);
        let (w, h) = bounds(spec, o.size);
        let mut s = scene.borrow_mut();
        let _ = s.advance(Instant::now(), 1.0);
        let size = s.layout(w, h);
        let list = s.draw_list(1.0);
        let (pw, ph) = (size.0.ceil().max(1.0) as u32, size.1.ceil().max(1.0) as u32);
        let name = format!("{n}-{}", spec.namespace);
        let mut picture = serde_json::Value::Null;
        if present {
            let rgba = renderer.snapshot(pw, ph, list)?;
            let path = o.out.join(format!("{name}.png"));
            write_png(&path, pw, ph, &rgba)?;
            picture = serde_json::Value::String(path.display().to_string());
            drawn += 1;
        }
        let nodes: Vec<serde_json::Value> = s
            .inspect()
            .into_iter()
            .map(|n| {
                serde_json::json!({
                    "node": n.node, "parent": n.parent, "depth": n.depth, "kind": n.kind, "role": n.role,
                    "name": n.name, "id": n.id, "text": n.text, "bounds": n.bounds, "present": n.present, "props": n.props,
                })
            })
            .collect();
        tree.push(serde_json::json!({
            "surface": n, "namespace": spec.namespace, "present": present, "size": [pw, ph],
            "picture": picture, "nodes": nodes,
        }));
    }
    let json = serde_json::to_string_pretty(&tree).map_err(|e| e.to_string())?;
    std::fs::write(o.out.join("tree.json"), json + "\n").map_err(|e| e.to_string())?;
    Ok(drawn)
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(anchor: &[&str], full: bool) -> SurfaceSpec {
        SurfaceSpec {
            anchor: anchor.iter().map(|a| (*a).to_owned()).collect(),
            margin: [0; 4],
            layer: "top".into(),
            output: None,
            reserve: false,
            reserve_strip: None,
            full,
            above_zones: false,
            outputs: None,
            all_outputs: false,
            namespace: "t".into(),
            popup: None,
            window: None,
            keyboard: "none".into(),
        }
    }

    #[test]
    fn a_surface_is_laid_out_as_the_compositor_would_give_it() {
        let screen = (1920, 1080);
        assert_eq!(bounds(&spec(&["top", "left", "right"], false), screen), (Some(1920.0), None), "a bar spans its edge");
        assert_eq!(bounds(&spec(&["top"], false), screen), (None, None), "a corner piece is its content");
        assert_eq!(bounds(&spec(&[], true), screen), (Some(1920.0), Some(1080.0)), "a full one is the screen");
    }

    #[test]
    fn options_read_as_written() {
        let o = options(["shell", "--size", "800x600", "--ipc", "cell toggle clock", "--wait", "0.5"].map(String::from).to_vec()).unwrap();
        assert_eq!((o.dir, o.size, o.wait), (PathBuf::from("shell"), (800, 600), 0.5));
        assert_eq!(o.steps, vec![Step::Ipc(vec!["cell".to_owned(), "toggle".into(), "clock".into()])]);
        let o = options(["s", "--click", "ok", "--sleep", "1", "--mock", "@genoma/vitals=v.luau"].map(String::from).to_vec()).unwrap();
        assert_eq!(o.steps, vec![Step::Click("ok".into()), Step::Sleep(1.0)]);
        assert_eq!(o.mocks, vec![("@genoma/vitals".to_owned(), PathBuf::from("v.luau"))]);
        assert!(options(vec!["--size".into()]).is_err());
        assert!(options(vec![]).unwrap_err().contains("the interface"));
        assert!(!on_screen(&SurfaceSpec { output: Some("DP-1".into()), ..spec(&[], false) }, "SNAP"));
    }
}
