//! Reproducible bounded-memory storage benchmark. Run with `cargo run --release
//! --example storage_bench -- <ordinary|dense|long> <MiB> <copy|clone>`.
//! The input and output are real allocated text files in an isolated temp directory.

use ede::document::{
    ByteOffset, Cancellation, Document, Edit, OpenOptions, OpenProgress, SaveOutcome, SaveTarget,
    TextRange,
};
use std::{
    error::Error,
    fs::File,
    io::{Read, Write},
    time::{Duration, Instant},
};

/// Generate a text workload without constructing a file-sized allocation.
fn generate(file: &mut File, pattern: &[u8], length: u64) -> std::io::Result<()> {
    let mut chunk = vec![0; 1 << 20];
    for (index, byte) in chunk.iter_mut().enumerate() {
        *byte = pattern[index % pattern.len()];
    }
    let mut remaining = length;
    while remaining > 0 {
        let count = remaining.min(chunk.len() as u64) as usize;
        file.write_all(&chunk[..count])?;
        remaining -= count as u64;
    }
    file.sync_all()
}
/// Wall milliseconds with submillisecond resolution for ordinary reads/edits.
fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
/// Measure peak process RSS and CPU using the OS, independently of managed cache accounting.
fn usage() -> Result<(u64, f64), std::io::Error> {
    let mut raw = std::mem::MaybeUninit::<nix::libc::rusage>::uninit();
    // SAFETY: valid output allocation and current-process selector.
    if unsafe { nix::libc::getrusage(nix::libc::RUSAGE_SELF, raw.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful getrusage initialized every field.
    let raw = unsafe { raw.assume_init() };
    #[cfg(target_os = "macos")]
    let rss = raw.ru_maxrss as u64;
    #[cfg(target_os = "linux")]
    let rss = raw.ru_maxrss as u64 * 1024;
    let cpu = (raw.ru_utime.tv_sec + raw.ru_stime.tv_sec) as f64
        + (raw.ru_utime.tv_usec + raw.ru_stime.tv_usec) as f64 / 1e6;
    Ok((rss, cpu))
}
/// Parse benchmark parameters, run open/read/edit/undo/save, and verify streamed output bytes.
fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let kind = arguments.next().unwrap_or_else(|| "ordinary".into());
    let mib: u64 = arguments.next().unwrap_or_else(|| "1024".into()).parse()?;
    let mode = arguments.next().unwrap_or_else(|| "copy".into());
    let pattern: &[u8] = match kind.as_str() {
        "ordinary" => b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012\n",
        "dense" => b"x\n",
        "long" => b"x",
        _ => return Err("unknown corpus".into()),
    };
    if !matches!(mode.as_str(), "copy" | "clone") || mib == 0 || arguments.next().is_some() {
        return Err("usage: storage_bench <ordinary|dense|long> <MiB> <copy|clone>".into());
    }
    let length = mib.checked_mul(1 << 20).ok_or("fixture length overflow")?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("input");
    generate(&mut File::create(&path)?, pattern, length)?;
    let cancel = Cancellation::default();
    let options = OpenOptions {
        working_directory: Some(directory.path().to_owned()),
        force_copy: mode == "copy",
        ..OpenOptions::default()
    };
    let cpu_start = usage()?.1;
    let started = Instant::now();
    let mut preview = None;
    let mut document = Document::open(&path, options, &cancel, |progress| {
        if matches!(progress, OpenProgress::Preview { .. }) && preview.is_none() {
            preview = Some(started.elapsed());
        }
    })?;
    let ready = started.elapsed();
    let mut read_max = Duration::ZERO;
    let mut edit_max = Duration::ZERO;
    for position in [0, length / 2, length] {
        let snapshot = document.snapshot()?;
        let start = Instant::now();
        let window = snapshot.read_window(ByteOffset(position), 4096)?;
        drop(window);
        let start = start.elapsed();
        read_max = read_max.max(start);
        let started = Instant::now();
        let window = snapshot.read_window(ByteOffset(position), 4096)?;
        drop(window);
        println!(
            "read_position={position} first_ms={:.3} repeat_ms={:.3}",
            ms(start),
            ms(started.elapsed())
        );
        drop(snapshot);
        let started = Instant::now();
        document.apply(
            document.revision(),
            [Edit {
                range: TextRange::new(position, position)?,
                text: "edit\n",
            }],
            &cancel,
        )?;
        edit_max = edit_max.max(started.elapsed());
        let started = Instant::now();
        document.undo(document.revision())?;
        document.redo(document.revision())?;
        document.undo(document.revision())?;
        println!("undo_redo_undo_ms={:.3}", ms(started.elapsed()));
    }
    let snapshot = document.snapshot()?;
    let lines = snapshot.line_count();
    let started = Instant::now();
    let last = snapshot.locate_line(lines - 1, &cancel)?;
    println!(
        "lines={lines} last_line_byte={} last_line_lookup_ms={:.3}",
        last.0,
        ms(started.elapsed())
    );
    let started = Instant::now();
    let report = document.save(
        &snapshot,
        SaveTarget::New(directory.path().join("output")),
        &cancel,
    );
    let save = started.elapsed();
    if report.outcome != SaveOutcome::Saved {
        return Err(report
            .error
            .ok_or("unacknowledged save without error")?
            .into());
    }
    let mut input = File::open(&path)?;
    let mut output = File::open(directory.path().join("output"))?;
    let mut a = vec![0; 1 << 16];
    let mut b = vec![0; 1 << 16];
    loop {
        let count = input.read(&mut a)?;
        output.read_exact(&mut b[..count])?;
        if a[..count] != b[..count] {
            return Err("saved bytes mismatch".into());
        }
        if count == 0 {
            break;
        }
    }
    let mut extra = [0];
    if output.read(&mut extra)? != 0 {
        return Err("unexpected output suffix".into());
    }
    let stats = document.stats()?;
    let (rss, cpu) = usage()?;
    println!(
        "corpus={kind} bytes={length} mode={mode} actual_clone={} cache=warm-generated preview_ms={:.3} ready_index_ms={:.3} max_first_read_ms={:.3} max_edit_ms={:.3} save_ms={:.3} peak_rss_bytes={rss} managed_cache_bytes={} working_text_bytes={} working_metadata_bytes={} cpu_seconds={:.3}",
        stats.snapshot_cloned,
        ms(preview.ok_or("missing preview")?),
        ms(ready),
        ms(read_max),
        ms(edit_max),
        ms(save),
        stats.cache_bytes,
        stats.text_bytes,
        stats.metadata_bytes,
        cpu - cpu_start
    );
    Ok(())
}
