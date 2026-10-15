//! Bounded raw SSH capture. Disk/SQLite work is kept off async executor threads.
use super::*;
use std::sync::{Arc, Mutex};

struct Streams {
    started: bool,
    writers: Vec<OutputWriter>,
    outputs: Vec<StoredOutput>,
    truncated: bool,
    failed: bool,
}
pub(crate) struct Capture {
    repository: Repository,
    session: String,
    user: String,
    call: String,
    streams: Mutex<Streams>,
}
impl Capture {
    pub fn new(repository: Repository, session: String, user: String, call: String) -> Arc<Self> {
        Arc::new(Self {
            repository,
            session,
            user,
            call,
            streams: Mutex::new(Streams {
                started: false,
                writers: vec![],
                outputs: vec![],
                truncated: false,
                failed: false,
            }),
        })
    }
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut streams = this.streams.lock().map_err(|_| Error::Worker)?;
            if streams.started {
                return Err(Error::Conflict);
            }
            // Reserve both channels before sending exec. A quota failure cannot run a command.
            let mut writers = vec![];
            for channel in [OutputChannel::Stdout, OutputChannel::Stderr] {
                match this.repository.begin_output(
                    &this.session,
                    &this.user,
                    CaptureRequest {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        call_id: this.call.clone(),
                        channel,
                    },
                )? {
                    CaptureStart::New(writer) => writers.push(*writer),
                    CaptureStart::Existing(_) => return Err(Error::Conflict),
                }
            }
            streams.writers = writers;
            streams.started = true;
            Ok(())
        })
        .await
        .map_err(|_| Error::Worker)?
    }
    pub async fn write(self: &Arc<Self>, stderr: bool, bytes: Vec<u8>) {
        let this = self.clone();
        // Sequential SSH reads await each worker; there is no unbounded channel or detached drain.
        let _ = tokio::task::spawn_blocking(move || {
            let Ok(mut streams) = this.streams.lock() else {
                return;
            };
            if streams.failed || streams.writers.len() != 2 || !streams.started {
                return;
            }
            let writer = &mut streams.writers[usize::from(stderr)];
            let remaining = this
                .repository
                .output_limits
                .max_output_bytes
                .saturating_sub(writer.captured_bytes()) as usize;
            let take = remaining.min(bytes.len());
            if take > 0 && writer.write(&bytes[..take]).is_err() {
                streams.failed = true;
            }
            streams.truncated |= take < bytes.len();
        })
        .await;
    }
    pub fn finish(&self) -> Result<Vec<StoredOutput>> {
        let mut streams = self.streams.lock().map_err(|_| Error::Worker)?;
        let writers = std::mem::take(&mut streams.writers);
        for writer in writers {
            match writer.finish() {
                Ok(output) => streams.outputs.push(output),
                Err(_) => streams.failed = true,
            }
        }
        Ok(streams.outputs.clone())
    }
    pub fn status(&self) -> (bool, bool) {
        self.streams
            .lock()
            .map_or((true, true), |s| (s.truncated, s.failed))
    }
}
