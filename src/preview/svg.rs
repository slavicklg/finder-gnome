// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! SVG never enters GdkPixbuf/Glycin. Untrusted XML and rendering run in a
//! short-lived process with memory/CPU limits, no base URI, and a wall deadline.
use super::{Raster, tr};
use gio::prelude::*;
use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const INPUT_LIMIT: usize = 4 * 1024 * 1024;
const PIXEL_LIMIT: u64 = 40_000_000;
const OUTPUT_LIMIT: usize = 16 + PIXEL_LIMIT as usize * 4 + 1024;
const DEADLINE: Duration = Duration::from_secs(5);
const OK: &[u8; 8] = b"FGSVG001";
const ERROR: &[u8; 8] = b"FGSVERR1";
const INPUT: &[u8; 8] = b"FGSVGIN1";

/// Sniff XML as well as the declared type: an SVG named .png must not reach
/// file_info(). PNG/JPEG/etc retain the existing Pixbuf decoding path.
pub(super) fn is_svg(
    file: &gio::File,
    mime: &str,
    cancel: &gio::Cancellable,
) -> Result<bool, String> {
    let name = file
        .basename()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if name.ends_with(".svgz") || mime == "image/svg+xml-compressed" {
        return Err(tr("Compressed SVG previews are not supported."));
    }
    if mime.starts_with("image/svg") || name.ends_with(".svg") {
        return Ok(true);
    }
    let stream = file.read(Some(cancel)).map_err(|e| e.to_string())?;
    let bytes = stream
        .read_bytes(4096, Some(cancel))
        .map_err(|e| e.to_string())?;
    let bytes = bytes.as_ref();
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Err(tr("Compressed SVG previews are not supported."));
    }
    // Recognize XML encodings before any raster-format probing.
    if bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || bytes.starts_with(&[0xff, 0xfe])
        || bytes.starts_with(&[0xfe, 0xff])
        || bytes.starts_with(&[0, 0, 0xfe, 0xff])
        || bytes.starts_with(&[0, b'<'])
        || bytes.starts_with(&[b'<', 0])
        || bytes.starts_with(&[0, 0, 0, b'<'])
    {
        return Ok(true);
    }
    Ok(bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_none_or(|b| *b == b'<'))
}

pub(super) fn load(
    file: &gio::File,
    extent: i32,
    cancel: &gio::Cancellable,
) -> Result<Raster, String> {
    let stream = file.read(Some(cancel)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    loop {
        let part = stream
            .read_bytes((INPUT_LIMIT + 1 - bytes.len()).min(64 * 1024), Some(cancel))
            .map_err(|e| e.to_string())?;
        if part.is_empty() {
            break;
        }
        bytes.extend_from_slice(&part);
        if bytes.len() > INPUT_LIMIT {
            return Err(tr("SVG exceeds the 4 MiB input limit."));
        }
    }
    render(bytes, extent, cancel)
}

fn render(bytes: Vec<u8>, extent: i32, cancel: &gio::Cancellable) -> Result<Raster, String> {
    if cancel.is_cancelled() {
        return Err(tr("Preview cancelled."));
    }
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    #[cfg(not(test))]
    command.arg("--finder-gnome-render-svg");
    #[cfg(test)]
    command
        .args([
            "--exact",
            "preview::svg::tests::helper_process",
            "--ignored",
            "--nocapture",
        ])
        .env("FINDER_GNOME_SVG_HELPER", "1");
    run_worker(command, bytes, extent, cancel, DEADLINE)
}

fn run_worker(
    mut command: Command,
    bytes: Vec<u8>,
    extent: i32,
    cancel: &gio::Cancellable,
    timeout: Duration,
) -> Result<Raster, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Only async-signal-safe syscalls are used between fork and exec. Give this
    // worker its own process group so a timeout also terminates loader children.
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for (resource, maximum) in [
                (libc::RLIMIT_AS, 768 * 1024 * 1024),
                (libc::RLIMIT_CPU, 3),
                (libc::RLIMIT_NOFILE, 64),
            ] {
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(resource, &mut limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                limit.rlim_cur = limit.rlim_cur.min(maximum);
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        stdin.write_all(INPUT)?;
        stdin.write_all(&extent.to_le_bytes())?;
        stdin.write_all(&bytes)
    });
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(OUTPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        // Discard bounded diagnostic output; SVG text is never logged.
        let _ = std::io::copy(&mut stderr.take(16 * 1024), &mut std::io::sink());
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if !cancel.is_cancelled() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => break None,
        }
    };
    // SAFETY: pre_exec established a new process group whose ID is this child.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.wait();
    let _ = writer.join();
    let output = reader
        .join()
        .map_err(|_| tr("SVG renderer failed."))?
        .map_err(|e| e.to_string())?;
    let _ = errors.join();
    if cancel.is_cancelled() {
        return Err(tr("Preview cancelled."));
    }
    if !status.is_some_and(|status| status.success()) {
        return Err(tr(
            "SVG preview exceeded its resource limit or the renderer failed.",
        ));
    }
    decode(output)
}

fn decode(mut bytes: Vec<u8>) -> Result<Raster, String> {
    // A test subprocess has a small libtest preamble. Production has none.
    let start = bytes
        .windows(8)
        .take(256)
        .position(|value| value == OK || value == ERROR)
        .ok_or_else(|| tr("Invalid SVG renderer response."))?;
    if &bytes[start..start + 8] == ERROR {
        return Err(String::from_utf8_lossy(&bytes[start + 8..])
            .chars()
            .take(8192)
            .collect());
    }
    let header = bytes
        .get(start + 8..start + 16)
        .ok_or_else(|| tr("Invalid SVG renderer response."))?;
    let width = u32::from_le_bytes(header[..4].try_into().unwrap());
    let height = u32::from_le_bytes(header[4..].try_into().unwrap());
    let length = u64::from(width) * u64::from(height) * 4;
    if width == 0
        || height == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
        || length > PIXEL_LIMIT * 4
        || bytes.len() as u64 != start as u64 + 16 + length
    {
        return Err(tr("Invalid SVG renderer dimensions."));
    }
    bytes.drain(..start + 16);
    Ok(Raster {
        width: width as i32,
        height: height as i32,
        pixels: bytes,
        pages: 0,
    })
}

/// Internal subprocess entry point; called before GTK/application startup.
pub(crate) fn helper_main() -> i32 {
    let result = (|| {
        let mut input = Vec::new();
        std::io::stdin()
            .take(INPUT_LIMIT as u64 + 13)
            .read_to_end(&mut input)
            .map_err(|e| e.to_string())?;
        if input.len() < 12 || input.len() > INPUT_LIMIT + 12 || &input[..8] != INPUT {
            return Err(tr("Invalid or oversized SVG input."));
        }
        let extent = i32::from_le_bytes(input[8..12].try_into().unwrap());
        renderer::render(&input[12..], extent)
    })();
    let mut output = std::io::stdout().lock();
    let written = match result {
        Ok(raster) => output
            .write_all(OK)
            .and_then(|_| output.write_all(&(raster.width as u32).to_le_bytes()))
            .and_then(|_| output.write_all(&(raster.height as u32).to_le_bytes()))
            .and_then(|_| output.write_all(&raster.pixels)),
        Err(error) => output
            .write_all(ERROR)
            .and_then(|_| output.write_all(&error.as_bytes()[..error.len().min(8192)])),
    };
    if written.and_then(|_| output.flush()).is_ok() {
        0
    } else {
        1
    }
}

mod renderer;
#[cfg(test)]
mod tests;
