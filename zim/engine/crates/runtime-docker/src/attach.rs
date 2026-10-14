//! The console attach pump (M7.1): the runtime side of the console
//! pipe.
//!
//! [`attach_console`](crate::DockerRuntime) opens bollard's container
//! attach (stdin + stdout + stderr, live stream only — no historical
//! log replay, matching Wings's attach behavior), then:
//!
//! - spawns [`ConsolePump`], which reads the attach stream's frames,
//!   re-segments them on newline boundaries ([`LineAssembler`]), and
//!   forwards each decoded line over a bounded channel — the
//!   [`ConsoleAttachment`](zim_engine_application::supervision::ConsoleAttachment)
//!   the supervision layer consumes and republishes onto the
//!   streaming hub;
//! - parks the attach's stdin writer in [`ConsoleState`], so
//!   `Runtime::send_input` writes to the container's stdin through
//!   the *same* persistent attachment (a one-shot attach would send
//!   EOF to the container's stdin when dropped — the SIGHUP risk the
//!   M6 surface avoided by answering `Unsupported`).
//!
//! # Frame shapes, documented per the M7.1 contract
//!
//! ZIM containers are TTY containers (`tty: true`, Wings parity), so
//! the daemon's attach output is a raw terminal byte stream and
//! bollard decodes each chunk as a single `Console` frame — chunks
//! are **not** line-aligned. Non-TTY attach output would arrive
//! multiplexed (`StdOut`/`StdErr` frames, frame-aligned only). Either
//! way the assembler re-segments on `\n` boundaries: lines keep their
//! newline as received (a TTY's `\r\n` included), decoding is lossy,
//! and a target that dies mid-line has its unterminated tail flushed
//! at stream end.
//!
//! # Lifetime
//!
//! The pump ends when the attach stream ends (the container exited —
//! bollard surfaces some daemon versions' clean exits as stream
//! errors, which are treated identically) or when the supervision
//! consumer drops the receiver. Either way the pump flushes its
//! partial-line tail (when a consumer remains) and detaches the stdin
//! writer, after which `send_input` answers
//! [`RuntimeError::NotRunning`] until the next attach.

use std::pin::Pin;
use std::sync::Arc;

use bollard::container::LogOutput;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};

use zim_engine_application::supervision::{ConsoleAttachment, RuntimeError};

use crate::container;
use crate::{map_error, Docker};

/// The attach-stream type bollard hands back (`LogOutput` frames).
type AttachOutput =
    Pin<Box<dyn futures_util::Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>;
/// The attach's stdin writer type.
type AttachInput = Pin<Box<dyn tokio::io::AsyncWrite + Send>>;

/// The per-instance console attachment state, shared between the
/// runtime handle (`send_input`) and the pump task (detach on end).
pub(crate) struct ConsoleState {
    /// The live attachment's stdin writer; `None` while detached.
    input: Mutex<Option<AttachInput>>,
}

impl ConsoleState {
    /// Empty (detached) state.
    pub(crate) fn shared() -> Arc<Self> {
        Arc::new(Self {
            input: Mutex::new(None),
        })
    }
}

/// The console channel's capacity, in lines.
///
/// Bound chosen so a stalling supervision consumer applies
/// backpressure to the container's output stream (the pump awaits
/// `send`) without ever blocking the runtime's lifecycle calls — the
/// pump is its own task, and the Docker daemon's attach buffer
/// (default ~64 KiB) absorbs the in-flight gap. At the §13 console
/// line scale (≤ 64 KiB lines) the channel's worst-case footprint
/// stays in single-digit megabytes.
const CONSOLE_CHANNEL_CAPACITY: usize = 256;

/// A line longer than this is emitted as-is (forced split): the
/// assembler's memory stays bounded even against a workload that
/// never emits a newline.
const MAX_PENDING_BYTES: usize = 64 * 1024;

/// Attach to the named container's console: verify it is running,
/// open the bollard attach, park the stdin writer, and spawn the
/// output pump. Returns the supervision-facing line channel.
pub(crate) async fn attach_console(
    docker: &Docker,
    name: &str,
    state: &Arc<ConsoleState>,
) -> Result<ConsoleAttachment, RuntimeError> {
    // The attach endpoint itself would reject a stopped container
    // with an opaque 409; the pre-check maps that onto the
    // supervision vocabulary's NotRunning form (and NotFound is the
    // same quiet case — a never-started or reaped container).
    let observed = container::state(docker, name).await?;
    if !observed.is_running() {
        return Err(RuntimeError::NotRunning(format!(
            "console attach: container {name} is not running"
        )));
    }

    let options = bollard::query_parameters::AttachContainerOptionsBuilder::default()
        .stdin(true)
        .stdout(true)
        .stderr(true)
        // Live output only: historical console output is not part of
        // the M7.1 surface (the hub's in-memory retention covers
        // reconnects; an engine restart clears it by design).
        .stream(true)
        .logs(false)
        .build();

    let attach = docker
        .attach_container(name, Some(options))
        .await
        .map_err(|e| map_error("console attach", e))?;

    let (tx, rx) = mpsc::channel::<String>(CONSOLE_CHANNEL_CAPACITY);
    {
        let mut slot = state.input.lock().await;
        // Replace any writer a previous (abandoned) attachment left
        // behind: its pump is gone, so its writer is unreachable.
        *slot = Some(attach.input);
    }

    let pump = ConsolePump {
        output: attach.output,
        tx,
        state: Arc::clone(state),
        assembler: LineAssembler::default(),
    };
    tokio::spawn(pump.run());

    Ok(ConsoleAttachment { lines: rx })
}

/// Map an stdin-write I/O failure onto the runtime vocabulary.
///
/// A broken pipe means the attachment closed underneath the write
/// (the container exited mid-write): the not-running form, so the
/// API answers the retryable 409 rather than a 500.
fn input_io_error(operation: &'static str, err: std::io::Error) -> RuntimeError {
    match err.kind() {
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected => {
            RuntimeError::NotRunning(format!("{operation}: the console attachment closed"))
        }
        _ => RuntimeError::Backend(format!("{operation}: {err}")),
    }
}

/// Write one console input payload through the live attachment.
///
/// The payload is newline-terminated when the caller's bytes are not
/// (line-oriented input: the TTY line discipline delivers on newline,
/// and the API's `line` field is newline-free by construction).
pub(crate) async fn send_input(
    state: &Arc<ConsoleState>,
    input: &[u8],
) -> Result<(), RuntimeError> {
    let mut slot = state.input.lock().await;
    let Some(writer) = slot.as_mut() else {
        return Err(RuntimeError::NotRunning(
            "console input: no live attachment (the container is not running)".to_owned(),
        ));
    };

    let mut line = Vec::with_capacity(input.len() + 1);
    line.extend_from_slice(input);
    if !line.ends_with(b"\n") {
        line.push(b'\n');
    }
    writer
        .write_all(&line)
        .await
        .map_err(|e| input_io_error("console input", e))?;
    writer
        .flush()
        .await
        .map_err(|e| input_io_error("console input flush", e))?;
    Ok(())
}

/// The runtime-side output pump: attach stream frames → assembled,
/// decoded lines → the supervision-facing channel.
struct ConsolePump {
    output: AttachOutput,
    tx: mpsc::Sender<String>,
    state: Arc<ConsoleState>,
    assembler: LineAssembler,
}

impl ConsolePump {
    /// Read the attach stream to its end, forwarding lines; then
    /// flush the partial-line tail and detach the stdin writer.
    async fn run(mut self) {
        let mut consumer_gone = false;
        while !consumer_gone {
            let Some(item) = self.output.next().await else {
                break; // stream end: the container exited
            };
            let frame = match item {
                Ok(frame) => frame,
                Err(_) => {
                    // Some daemons surface a clean container exit as
                    // a stream error; either way the attachment is
                    // over. Treat identically to stream end.
                    break;
                }
            };
            let bytes = match frame {
                LogOutput::StdOut { message }
                | LogOutput::StdErr { message }
                | LogOutput::Console { message } => message,
                // The attach stream never carries StdIn frames back;
                // skip defensively.
                LogOutput::StdIn { .. } => continue,
            };
            for line in self.assembler.push(bytes.as_ref()) {
                if self.tx.send(line).await.is_err() {
                    // The supervision consumer dropped the receiver
                    // (pump replaced or instance shutdown): stop.
                    consumer_gone = true;
                    break;
                }
            }
        }

        // Flush the unterminated tail when a consumer remains, then
        // detach stdin either way (send_input answers NotRunning
        // until the next attach).
        if !consumer_gone {
            if let Some(tail) = self.assembler.flush_eof() {
                let _ = self.tx.send(tail).await;
            }
        }
        *self.state.input.lock().await = None;
    }
}

/// Newline-bounded line assembly over raw attach-stream chunks.
///
/// TTY `Console` frames are not line-aligned (the daemon hands over
/// whatever the terminal produced) and multiplexed `StdOut`/`StdErr`
/// frames are frame-aligned only, so the assembler re-segments the
/// byte stream on `\n` boundaries. Lines keep their newline as
/// received; decoding is lossy per the M7.1 contract.
#[derive(Default)]
struct LineAssembler {
    /// The unterminated bytes carried between frames.
    pending: Vec<u8>,
}

impl LineAssembler {
    /// Absorb one chunk; return every line it completes.
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        // Bound the assembler's memory against a workload that never
        // emits a newline: emit the oversized tail as its own line
        // (unterminated — the forced split is visible as such).
        if self.pending.len() > MAX_PENDING_BYTES {
            let forced = std::mem::take(&mut self.pending);
            lines.push(String::from_utf8_lossy(&forced).into_owned());
        }
        lines
    }

    /// The unterminated tail at stream end, if any (the target died
    /// mid-line; flushed without a newline). Takes the buffer: a
    /// second call is empty.
    fn flush_eof(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame-boundary independence: one logical line split across
    /// many chunks assembles into one line, newline kept.
    #[test]
    fn line_assembler_splits_on_newlines_not_frames() {
        let mut assembler = LineAssembler::default();
        assert!(assembler.push(b"hel").is_empty());
        assert!(assembler.push(b"lo").is_empty());
        let lines = assembler.push(b" world\n");
        assert_eq!(lines, ["hello world\n"]);
        assert!(assembler.flush_eof().is_none());
    }

    /// One chunk carrying many lines yields them in order.
    #[test]
    fn line_assembler_multi_line_chunk() {
        let mut assembler = LineAssembler::default();
        let lines = assembler.push(b"one\ntwo\nthree\n");
        assert_eq!(lines, ["one\n", "two\n", "three\n"]);
    }

    /// A TTY's `\r\n` line endings pass through verbatim.
    #[test]
    fn line_assembler_keeps_crlf() {
        let mut assembler = LineAssembler::default();
        let lines = assembler.push(b"booting\r\nready\r\n");
        assert_eq!(lines, ["booting\r\n", "ready\r\n"]);
    }

    /// The EOF tail flushes without a synthesized newline.
    #[test]
    fn line_assembler_flushes_partial_tail_at_eof() {
        let mut assembler = LineAssembler::default();
        assert_eq!(assembler.push(b"done\npartial tai"), ["done\n"]);
        assert_eq!(assembler.flush_eof(), Some("partial tai".to_owned()));
        assert!(assembler.flush_eof().is_none());
    }

    /// Invalid UTF-8 decodes lossily (replacement characters), per
    /// the M7.1 "lossy UTF-8 per line" contract.
    #[test]
    fn line_assembler_decodes_lossily() {
        let mut assembler = LineAssembler::default();
        let lines = assembler.push(b"\xff\xfe garbage \x80\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("garbage"));
        assert!(lines[0].contains('\u{FFFD}'));
    }

    /// A runaway writer (no newline ever) gets force-split at the
    /// cap, keeping the assembler's memory bounded.
    #[test]
    fn line_assembler_bounds_runaway_lines() {
        let mut assembler = LineAssembler::default();
        let chunk = vec![b'x'; MAX_PENDING_BYTES / 2 + 8];
        assert!(assembler.push(&chunk).is_empty());
        let lines = assembler.push(&chunk);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_PENDING_BYTES + 16);
        assert!(assembler.pending.is_empty());
    }
}
