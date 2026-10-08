// SPDX-License-Identifier: BSD-3-Clause
//! Drain child output without giving it the diagnostic parent's bounded file FD.
use std::{
    collections::VecDeque,
    io::{self, Read},
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Command, Stdio},
    time::Instant,
};

const CAPTURE_BYTES: usize = 16 * 1024;
const TAIL_BYTES: usize = 8 * 1024;
const POLL_BYTES: usize = 64 * 1024;

#[cfg(feature = "diag-cycle")]
#[derive(serde::Serialize)]
struct Chunk {
    ms: u64,
    stream_offset: u64,
    bytes: usize,
}

#[cfg(feature = "diag-cycle")]
struct Timeline {
    started: Instant,
    clock: &'static str,
    head: Vec<Chunk>,
    tail: VecDeque<Chunk>,
    seen: u64,
}

pub(crate) struct ChildLog {
    reader: UnixStream,
    bytes: Vec<u8>,
    tail: VecDeque<u8>,
    eof: bool,
    tail_limit: usize,
    received: u64,
    error: Option<io::Error>,
    pid: Option<u32>,
    #[cfg(feature = "diag-cycle")]
    timeline: Option<Timeline>,
}

impl ChildLog {
    pub(crate) fn attach(command: &mut Command, keep_tail: bool) -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        command
            .stdout(Stdio::from(OwnedFd::from(writer.try_clone()?)))
            .stderr(Stdio::from(OwnedFd::from(writer)));
        Ok(Self {
            reader,
            bytes: Vec::with_capacity(CAPTURE_BYTES),
            tail: VecDeque::new(),
            eof: false,
            tail_limit: if keep_tail { TAIL_BYTES } else { 0 },
            received: 0,
            error: None,
            pid: None,
            #[cfg(feature = "diag-cycle")]
            timeline: None,
        })
    }

    pub(crate) fn observe(&mut self, pid: u32, _started: Option<Instant>, _clock: &'static str) {
        self.pid = Some(pid);
        #[cfg(feature = "diag-cycle")]
        { self.timeline = _started.map(|started| Timeline {
            started, clock: _clock, head: Vec::new(), tail: VecDeque::new(), seen: 0,
        }); }
    }

    pub(crate) fn poll(&mut self) {
        if self.error.is_some() {
            return;
        }
        let mut buffer = [0u8; 4096];
        // Keep draining discarded bytes, but do not starve IPC/cancellation.
        for _ in 0..POLL_BYTES / buffer.len() {
            match self.reader.read(&mut buffer) {
                Ok(0) => { self.eof = true; break; },
                Ok(count) => {
                    #[cfg(feature = "diag-cycle")]
                    if let Some(timeline) = &mut self.timeline {
                        let chunk = Chunk { ms: timeline.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                            stream_offset: self.received, bytes: count };
                        timeline.seen = timeline.seen.saturating_add(1);
                        if timeline.head.len() < 32 { timeline.head.push(chunk); }
                        else {
                            if timeline.tail.len() == 32 { timeline.tail.pop_front(); }
                            timeline.tail.push_back(chunk);
                        }
                    }
                    self.received = self.received.saturating_add(count as u64);
                    let kept = count.min(CAPTURE_BYTES - self.bytes.len());
                    self.bytes.extend_from_slice(&buffer[..kept]);
                    if self.tail_limit > 0 {
                        self.tail.extend(&buffer[kept..count]);
                        if self.tail.len() > self.tail_limit {
                            self.tail.drain(..self.tail.len() - self.tail_limit);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.error = Some(error);
                    break;
                }
            }
        }
    }

    pub(crate) fn report(mut self) -> (Vec<u8>, serde_json::Value) {
        // Bounded final drain: descendants holding the socket must not hang cleanup.
        for _ in 0..4 {
            if self.eof || self.error.is_some() { break; }
            self.poll();
        }
        let omitted = self.received.saturating_sub((self.bytes.len() + self.tail.len()) as u64);
        #[allow(unused_mut)]
        let mut metadata = serde_json::json!({
            "pid":self.pid, "streams":"merged_stdout_stderr",
            "head_stream_offset":0, "tail_stream_offset":self.received.saturating_sub(self.tail.len() as u64),
            "received_bytes":self.received, "head_bytes":self.bytes.len(), "tail_bytes":self.tail.len(),
            "omitted_bytes":omitted, "drain_complete":self.eof,
            "error":self.error.as_ref().map(ToString::to_string),
        });
        #[cfg(feature = "diag-cycle")]
        if let Some(timeline) = self.timeline {
            metadata["timing"] = serde_json::json!({
                "clock":timeline.clock, "meaning":"receipt_not_emission", "unit":"milliseconds",
                "chunks_seen":timeline.seen,
                "chunks_dropped":timeline.seen.saturating_sub((timeline.head.len() + timeline.tail.len()) as u64),
                "chunks":timeline.head.into_iter().chain(timeline.tail).collect::<Vec<_>>(),
            });
        }
        if omitted > 0 {
            self.bytes.extend_from_slice(format!("\n[... {omitted} bytes omitted ...]\n").as_bytes());
        }
        self.bytes.extend(self.tail);
        (self.bytes, metadata)
    }
}

/// Output metadata survives a full manager text log through the structured segment.
pub(crate) fn finish_gui(log: ChildLog, _control: Option<&mut crate::session::control::Control>) {
    let (bytes, metadata) = log.report();
    #[cfg(feature = "diag-cycle")]
    if let Some(segment) = _control.and_then(|c| c.cycle_segment()) {
        segment.gui_output = metadata.clone();
    }
    crate::session::diagnostic(format_args!("iman: IGUI output received={}, omitted={}, drain_complete={}, error={}\n{}",
        metadata["received_bytes"], metadata["omitted_bytes"], metadata["drain_complete"], metadata["error"],
        String::from_utf8_lossy(&bytes)));
}
