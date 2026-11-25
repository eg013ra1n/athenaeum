//! Lab probe of the header text (starloom phase 2, Task 4 facts gate): the text rustafits' reader reads from a file,
//! as one line `<path>\t<byte length>\t<xxh3-64 hex>` per argument, for `athenaeum-lab`'s `p2-facts.py` to hold
//! against the text the catalog stored.
//!
//! ```text
//! cargo run --release -p athenaeum-core --example rustafits_header_text -- <file>...
//! cargo run --release -p athenaeum-core --example rustafits_header_text -- --hash-stdin   # NUL-separated texts on stdin
//! ```
//!
//! `--hash-stdin` prints `<byte length>\t<xxh3-64 hex>` for each NUL-separated text on stdin, with the same hash, so
//! the script hashes the stored text exactly as this probe hashes the read one. A file that cannot be read prints
//! `<path>\terror\t<message>` and the run goes on; the exit code is 1 when any did.

use astroimage::header::header_text;
use std::io::{Read, Write};
use std::path::Path;
use xxhash_rust::xxh3::xxh3_64;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    if args.len() == 1 && args[0] == "--hash-stdin" {
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin().lock().read_to_end(&mut buf) {
            tracing::error!(error = %e, "hash-stdin: read failed");
            std::process::exit(2);
        }
        // `writeln!` on stdout fails only on a closed pipe; the run ends there.
        for text in buf.split(|b| *b == 0) {
            if writeln!(out, "{}\t{:016x}", text.len(), xxh3_64(text)).is_err() {
                std::process::exit(2);
            }
        }
        return;
    }
    let mut failed = false;
    for arg in &args {
        match header_text(Path::new(arg)) {
            Ok((_, text)) => {
                if writeln!(out, "{arg}\t{}\t{:016x}", text.len(), xxh3_64(text.as_bytes())).is_err() {
                    std::process::exit(2);
                }
            }
            Err(e) => {
                failed = true;
                tracing::warn!(path = %arg, error = %e, "header text unreadable");
                let message = e.to_string().replace(['\t', '\n'], " ");
                if writeln!(out, "{arg}\terror\t{message}").is_err() {
                    std::process::exit(2);
                }
            }
        }
    }
    let _ = out.flush();
    if failed {
        std::process::exit(1);
    }
}
