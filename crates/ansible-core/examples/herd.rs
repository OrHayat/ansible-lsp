//! TEMPORARY (uncommitted) bench for the cold-cache thundering herd.
//!
//! Mimics the LSP's startup scan: one `ScanCache` shared by N threads, each var-indexing a
//! file. The single-threaded `scan` binary can never show this — the duplicate work only
//! happens when many threads miss the same cold key at once.
//!
//! `cargo run --release --example herd -- <dir> [threads] [repeats]`

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ansible_core::cache::ScanCache;
use ansible_core::parse::Document;
use ansible_core::workspace::yaml_files;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or_else(|| "demo".into()));
    let threads: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or_else(num_cpus);
    let repeats: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);

    let files: Vec<PathBuf> = yaml_files(&dir);
    println!("{} files, {threads} threads, {repeats} runs", files.len());

    let mut best = f64::MAX;
    let mut worst: f64 = 0.0;
    for run in 0..repeats {
        // A fresh cache per run: the cold pass is the whole point.
        let cache = Arc::new(ScanCache::default());
        let next = Arc::new(AtomicUsize::new(0));
        let started = std::time::Instant::now();
        std::thread::scope(|s| {
            for _ in 0..threads {
                let (cache, next, files) = (cache.clone(), next.clone(), &files);
                s.spawn(move || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(i) else { return };
                    let Ok(text) = std::fs::read_to_string(path) else { continue };
                    let doc = Document::new(text);
                    let Some(nodes) = doc.parse() else { continue };
                    let _ = ansible_core::vars::undefined_uses_in(path, &nodes, &doc.text, &cache);
                });
            }
        });
        let ms = started.elapsed().as_secs_f64() * 1e3;
        best = best.min(ms);
        worst = worst.max(ms);
        let st = cache.stats();
        println!(
            "run {run}: {ms:7.1} ms   reads {:4}  contexts {:3}  ansible.cfg {:3}  \
             edges {:4} -> {:3} files",
            st.reads, st.contexts, st.configs, st.edges, st.files
        );
    }
    println!("best {best:.1} ms, worst {worst:.1} ms");
}

fn num_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}
