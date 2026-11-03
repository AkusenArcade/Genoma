//! `GENOMA_PROFILE=1`: where the main thread's time in Luau goes. Every
//! binding, computed, effect and watch is timed and counted by the place
//! its function is defined; every two seconds with something to say, the
//! most expensive are logged. perf sees only `luau_execute`; this names the
//! Luau. Nothing is measured without the variable.

use std::{
    cell::RefCell,
    collections::HashMap,
    time::{Duration, Instant},
};

use mlua::Function;

thread_local! {
    static ON: bool = std::env::var_os("GENOMA_PROFILE").is_some();
    static TALLY: RefCell<(HashMap<String, (u32, Duration)>, Option<Instant>)> = RefCell::new((HashMap::new(), None));
}

/// `run` timed under `kind` and the place `f` is defined, when profiling.
pub(crate) fn timed<R>(kind: &str, f: &Function, run: impl FnOnce() -> R) -> R {
    if !ON.with(|on| *on) {
        return run();
    }
    let started = Instant::now();
    let result = run();
    let took = started.elapsed();
    let info = f.info();
    let place = format!(
        "{kind} {}:{}",
        info.short_src.as_deref().unwrap_or("?").rsplit('/').next().unwrap_or("?"),
        info.line_defined.unwrap_or(0)
    );
    TALLY.with(|t| {
        let mut t = t.borrow_mut();
        let entry = t.0.entry(place).or_default();
        entry.0 += 1;
        entry.1 += took;
        let since = *t.1.get_or_insert_with(Instant::now);
        if since.elapsed() >= Duration::from_secs(2) {
            let mut rows: Vec<_> = t.0.drain().collect();
            rows.sort_by(|a, b| b.1.1.cmp(&a.1.1));
            let total: Duration = rows.iter().map(|r| r.1.1).sum();
            if total >= Duration::from_millis(20) {
                log::info!("profile: {:.1} ms in Luau over the last {:.1} s", total.as_secs_f64() * 1000.0, since.elapsed().as_secs_f64());
                for (place, (count, time)) in rows.iter().take(20) {
                    log::info!("profile: {:8.2} ms {:6} × {place}", time.as_secs_f64() * 1000.0, count);
                }
            }
            t.1 = Some(Instant::now());
        }
    });
    result
}
