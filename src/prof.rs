use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ON: AtomicBool = AtomicBool::new(false);
static GPU_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn enable(flag: bool) {
    let env = std::env::var("BSP2IMG_TIMINGS").is_ok_and(|v| !v.is_empty() && v != "0");
    ON.store(flag || env, Ordering::Relaxed);
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn note(s: impl FnOnce() -> String) {
    if on() {
        println!("  [t] {}", s());
    }
}

pub struct Timer {
    name: &'static str,
    last: Instant,
}

impl Timer {
    pub fn new(name: &'static str) -> Timer {
        Timer { name, last: Instant::now() }
    }

    pub fn lap(&mut self, what: &str) {
        let now = Instant::now();
        if on() {
            println!("  [t] {}.{what} {:.3}s", self.name, (now - self.last).as_secs_f64());
        }
        self.last = now;
    }
}

pub fn gpu_error(s: String) {
    eprintln!("gpu error: {s}");
    if let Ok(mut v) = GPU_ERRORS.lock() {
        v.push(s);
    }
}

pub fn take_gpu_errors() -> Vec<String> {
    GPU_ERRORS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
}
