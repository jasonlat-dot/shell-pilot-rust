use super::*;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::io::Read;

pub enum CaptureStart {
    New(Box<OutputWriter>),
    /// Existing completion is a read-only receipt; do not consume or rerun the tool.
    Existing(Box<StoredOutput>),
}
/// A bounded capture sink. Drop aborts unpublished chunks and releases reservations.
/// All methods are synchronous; feed it from a blocking worker or a bounded tool-output bridge.
pub struct OutputWriter {
    repo: Repository,
    id: String,
    token: String,
    size: u64,
    persisted: u64,
    buffer: Vec<u8>,
    head: Vec<u8>,
    tail: Vec<u8>,
    hash: Sha256,
    done: bool,
}
impl OutputWriter {
    pub(super) fn new(repo: Repository, id: String, token: String) -> Self {
        Self {
            repo,
            id,
            token,
            size: 0,
            persisted: 0,
            buffer: Vec::with_capacity(OUTPUT_CHUNK_BYTES),
            head: Vec::new(),
            tail: Vec::new(),
            hash: Sha256::new(),
            done: false,
        }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn captured_bytes(&self) -> u64 {
        self.size
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let result = self.write_inner(bytes);
        if result.is_err() {
            self.abort();
        }
        result
    }
    fn write_inner(&mut self, mut bytes: &[u8]) -> Result<()> {
        if self.done {
            return Err(Error::OutputUnavailable);
        }
        if self
            .size
            .checked_add(bytes.len() as u64)
            .is_none_or(|size| size > self.repo.output_limits.max_output_bytes)
        {
            return Err(Error::OutputTooLarge);
        }
        self.touch()?;
        while !bytes.is_empty() {
            let count = (OUTPUT_CHUNK_BYTES - self.buffer.len()).min(bytes.len());
            let chunk = &bytes[..count];
            self.hash.update(chunk);
            self.size += count as u64;
            let head_count = (OUTPUT_PREVIEW_BYTES / 2 - self.head.len()).min(count);
            self.head.extend_from_slice(&chunk[..head_count]);
            let tail_limit = OUTPUT_PREVIEW_BYTES / 2;
            if chunk.len() >= tail_limit {
                self.tail.clear();
                self.tail
                    .extend_from_slice(&chunk[chunk.len() - tail_limit..]);
            } else {
                let excess = (self.tail.len() + chunk.len()).saturating_sub(tail_limit);
                self.tail.drain(..excess);
                self.tail.extend_from_slice(chunk);
            }
            self.buffer.extend_from_slice(chunk);
            bytes = &bytes[count..];
            if self.buffer.len() == OUTPUT_CHUNK_BYTES {
                self.flush()?
            }
        }
        Ok(())
    }
    /// Suitable for finite local readers. Network tools should feed write() through
    /// their existing async timeout/cancellation loop instead of blocking here.
    pub fn copy_from<R: Read>(&mut self, reader: &mut R) -> Result<()> {
        if self.done {
            return Err(Error::OutputUnavailable);
        }
        let mut buffer = vec![0; OUTPUT_CHUNK_BYTES];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.abort();
                    return Err(Error::OutputInput(error));
                }
            };
            if count == 0 {
                return Ok(());
            }
            self.write(&buffer[..count])?;
        }
    }
    pub fn cancel(mut self) {
        self.abort()
    }
    fn touch(&self) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        self.repo.db(|db|{
            if db.execute("UPDATE agent_outputs SET expires_at=?1 WHERE id=?2 AND writer_token=?3 AND state='writing' AND expires_at>?4",
                params![now+self.repo.output_limits.idle_millis,self.id,self.token,now])?!=1{
                return Err(Error::OutputUnavailable)
            }
            Ok(())
        })
    }
    fn flush(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let next = self.persisted + self.buffer.len() as u64;
        let index = self.persisted / OUTPUT_CHUNK_BYTES as u64;
        let digest = super::output_model::hex(&Sha256::digest(&self.buffer));
        let now = chrono::Utc::now().timestamp_millis();
        self.repo.db(|db|{
            let tx=db.transaction()?;
            if tx.execute("UPDATE agent_outputs SET size=?1,expires_at=?2 WHERE id=?3 AND writer_token=?4 AND state='writing' AND size=?5 AND expires_at>?6",
                params![next,now+self.repo.output_limits.idle_millis,self.id,self.token,self.persisted,now])?!=1{
                return Err(Error::OutputUnavailable)
            }
            tx.execute("INSERT INTO agent_output_chunks(output_id,chunk_index,data,sha256) VALUES(?1,?2,?3,?4)",
                params![self.id,index,self.buffer,digest])?;
            tx.commit()?;Ok(())
        })?;
        self.persisted = next;
        self.buffer.clear();
        Ok(())
    }
    pub fn finish(mut self) -> Result<StoredOutput> {
        if self.done {
            return Err(Error::OutputUnavailable);
        }
        self.touch()?;
        self.flush()?;
        let digest = super::output_model::hex(&self.hash.clone().finalize());
        let (preview, truncated, lossy) = self.preview();
        let now = chrono::Utc::now().timestamp_millis();
        let output=self.repo.db(|db|{
            let tx=db.transaction()?;
            if tx.execute("UPDATE agent_outputs SET state='complete',sha256=?1,preview=?2,preview_truncated=?3,preview_lossy=?4 WHERE id=?5 AND writer_token=?6 AND state='writing' AND size=?7 AND expires_at>?8",
                params![digest,preview,truncated,lossy,self.id,self.token,self.size,now])?!=1{
                return Err(Error::OutputUnavailable)
            }
            let session:String=tx.query_row("SELECT session_id FROM agent_outputs WHERE id=?1",[&self.id],|row|row.get(0))?;
            let output=super::output::read_complete(&tx,&session,&self.id)?;
            tx.commit()?;Ok(output)
        })?;
        self.done = true;
        Ok(output)
    }
    fn preview(&self) -> (String, bool, bool) {
        let mut raw = self.head.clone();
        let skipped = if self.size > OUTPUT_PREVIEW_BYTES as u64 {
            raw.extend_from_slice(b"\n[... omitted; read output by ID ...]\n");
            raw.extend_from_slice(&self.tail);
            true
        } else {
            let overlap = (self.head.len() + self.tail.len()).saturating_sub(self.size as usize);
            raw.extend_from_slice(&self.tail[overlap..]);
            false
        };
        let lossy = std::str::from_utf8(&raw).is_err();
        let text = String::from_utf8_lossy(&raw).into_owned();
        if text.len() <= OUTPUT_PREVIEW_BYTES + 80 {
            return (text, skipped, lossy);
        }
        let head_end = floor_boundary(&text, OUTPUT_PREVIEW_BYTES / 2);
        let tail_start = ceil_boundary(&text, text.len() - OUTPUT_PREVIEW_BYTES / 2);
        (
            format!(
                "{}\n[... omitted; read output by ID ...]\n{}",
                &text[..head_end],
                &text[tail_start..]
            ),
            true,
            lossy,
        )
    }
    fn abort(&mut self) {
        if self.done {
            return;
        }
        let result=self.repo.db(|db|{
            let tx=db.transaction()?;
            tx.execute("DELETE FROM agent_output_chunks WHERE output_id IN (SELECT id FROM agent_outputs WHERE id=?1 AND writer_token=?2 AND state='writing')",
                params![self.id,self.token])?;
            tx.execute("DELETE FROM agent_outputs WHERE id=?1 AND writer_token=?2 AND state='writing'",params![self.id,self.token])?;
            tx.commit()?;Ok(())
        });
        if result.is_err() {
            tracing::warn!(output_id=%self.id,"unpublished output cleanup deferred until expiry");
        }
        self.done = true;
    }
}
impl Drop for OutputWriter {
    fn drop(&mut self) {
        self.abort()
    }
}
fn floor_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1
    }
    index
}
fn ceil_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index += 1
    }
    index
}
