use std::io::Write;

/// Install a panic hook that emits a `tracing::error!` event (captured by the
/// JSONL file layer) and also writes a dedicated `crash.log` as a backup —
/// preserved from the pre-tracing logging implementation so a crash is still
/// diagnosable even if the tracing subscriber itself failed to write the
/// panic event (e.g. the non-blocking appender channel was already torn
/// down). Reads the resolved log directory via `super::get_path()` rather
/// than capturing it directly, since `install()` takes no arguments (module
/// split from the single-file original, which captured `dir` in a closure).
pub fn install() {
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC: {info}\nBacktrace:\n{bt}");
        tracing::error!(%info, "panic");

        if let Some(dir) = super::get_path() {
            let crash_path = dir.join("crash.log");
            let ts = timestamp();
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(crash_path)
                .and_then(|mut f| writeln!(f, "[{ts}] {msg}"));
        }
    }));
}

fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
}
