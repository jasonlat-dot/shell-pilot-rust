use super::repository::read_session;
use super::*;
use crate::store::Store;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;

impl Repository {
    pub fn with_output_limits(store: Arc<Store>, limits: OutputLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            store,
            output_limits: limits,
        })
    }
    pub fn begin_output(
        &self,
        id: &str,
        user_id: &str,
        request: CaptureRequest,
    ) -> Result<CaptureStart> {
        request.validate()?;
        self.db(|db| {
            let tx = db.transaction()?;
            read_session(&tx,id,user_id)?;
            recover_expired(&tx)?;
            if let Some((output_id,call_id,channel,state)) = tx.query_row(
                "SELECT id,call_id,channel,state FROM agent_outputs WHERE session_id=?1 AND request_id=?2",
                params![id,request.request_id], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional()? {
                if call_id!=request.call_id || channel!=request.channel.as_str() { return Err(Error::Conflict); }
                if state!="complete" { return Err(Error::OutputBusy); }
                let output=read_complete(&tx,id,&output_id)?;
                tx.commit()?;
                return Ok(CaptureStart::Existing(Box::new(output)));
            }
            let task_id: String = tx.query_row(
                "SELECT task_id FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_call'",
                params![id,request.call_id], |r|r.get(0)).optional()?.ok_or(Error::ToolProtocol)?;
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_result')",
                params![id,request.call_id],|r|r.get::<_,bool>(0))? { return Err(Error::Conflict); }
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_outputs WHERE session_id=?1 AND call_id=?2 AND channel=?3)",
                params![id,request.call_id,request.channel.as_str()],|r|r.get::<_,bool>(0))? { return Err(Error::Conflict); }
            let limits=&self.output_limits;
            let (total,count,writers):(u64,usize,usize)=tx.query_row(
                "SELECT COALESCE(SUM(CASE WHEN state='writing' THEN max_bytes ELSE size END),0),COUNT(*),COALESCE(SUM(state='writing'),0) FROM agent_outputs",
                [],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            let session_bytes:u64=tx.query_row(
                "SELECT COALESCE(SUM(CASE WHEN state='writing' THEN max_bytes ELSE size END),0) FROM agent_outputs WHERE session_id=?1",
                [id],|r|r.get(0))?;
            if writers>=limits.max_writers { return Err(Error::OutputBusy); }
            if count>=limits.max_outputs
                || total.checked_add(limits.max_output_bytes).is_none_or(|v|v>limits.max_total_bytes)
                || session_bytes.checked_add(limits.max_output_bytes).is_none_or(|v|v>limits.max_session_bytes) {
                return Err(Error::OutputQuota);
            }
            let output_id=uuid::Uuid::new_v4().to_string();
            let token=uuid::Uuid::new_v4().to_string();
            let expires=chrono::Utc::now().timestamp_millis()+limits.idle_millis;
            tx.execute("INSERT INTO agent_outputs(id,session_id,task_id,call_id,channel,request_id,writer_token,state,max_bytes,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'writing',?8,?9,?10)",
                params![output_id,id,task_id,request.call_id,request.channel.as_str(),request.request_id,token,
                    limits.max_output_bytes,expires,chrono::Utc::now().timestamp_millis()])?;
            tx.commit()?;
            Ok(CaptureStart::New(Box::new(OutputWriter::new(self.clone(), output_id, token))))
        })
    }
    /// Explicit maintenance also runs before admitting a new capture. Only expired,
    /// unpublished captures are reclaimed; another live writer is never recovered.
    pub fn recover_outputs(&self) -> Result<usize> {
        self.db(|db| {
            let tx = db.transaction()?;
            let count = recover_expired(&tx)?;
            tx.commit()?;
            Ok(count)
        })
    }
    pub fn output(&self, id: &str, user_id: &str, output_id: &str) -> Result<StoredOutput> {
        self.db(|db| {
            let tx = db.transaction()?;
            read_session(&tx, id, user_id)?;
            let output = read_complete(&tx, id, output_id)?;
            tx.commit()?;
            Ok(output)
        })
    }
    pub fn output_stats(&self, id: &str, user_id: &str) -> Result<OutputStats> {
        self.db(|db|{
            read_session(db,id,user_id)?;
            let (used,reserved,count,writers)=db.query_row(
                "SELECT COALESCE(SUM(CASE WHEN state='complete' THEN size ELSE 0 END),0),COALESCE(SUM(CASE WHEN state='writing' THEN max_bytes ELSE 0 END),0),COUNT(*),COALESCE(SUM(state='writing'),0) FROM agent_outputs WHERE session_id=?1",
                [id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            Ok(OutputStats{used_bytes:used,reserved_bytes:reserved,outputs:count,active_writers:writers})
        })
    }
    pub fn read_output_range(
        &self,
        id: &str,
        user_id: &str,
        output_id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<OutputRange> {
        if !(1..=MAX_OUTPUT_READ_BYTES).contains(&limit) {
            return Err(Error::Invalid(
                "output range limit must be 1..65536 bytes".into(),
            ));
        }
        self.db(|db|{
            let tx=db.transaction()?;read_session(&tx,id,user_id)?;
            let output=read_complete(&tx,id,output_id)?;
            let size=output.reference.size;
            if offset>size {return Err(Error::Invalid("output offset exceeds length".into()))}
            let end=offset.checked_add(limit as u64).ok_or(Error::Invalid("output offset overflow".into()))?.min(size);
            let mut bytes=Vec::with_capacity((end-offset) as usize);
            if end>offset {
                let first=offset/OUTPUT_CHUNK_BYTES as u64;
                let last=(end-1)/OUTPUT_CHUNK_BYTES as u64;
                let mut query=tx.prepare("SELECT chunk_index,data,sha256 FROM agent_output_chunks WHERE output_id=?1 AND chunk_index BETWEEN ?2 AND ?3 ORDER BY chunk_index")?;
                let mut rows=query.query(params![output_id,first,last])?;
                let mut expected=first;
                while let Some(row)=rows.next()? {
                    let index:u64=row.get(0)?; let data:Vec<u8>=row.get(1)?; let digest:String=row.get(2)?;
                    let start=index*OUTPUT_CHUNK_BYTES as u64;
                    let expected_size=(size-start).min(OUTPUT_CHUNK_BYTES as u64) as usize;
                    if index!=expected || data.len()!=expected_size || digest!=super::output_model::hex(&Sha256::digest(&data)) {
                        return Err(Error::OutputUnavailable)
                    }
                    let from=offset.saturating_sub(start) as usize;
                    let to=(end-start).min(data.len() as u64) as usize;
                    bytes.extend_from_slice(&data[from..to]);expected+=1;
                }
                if bytes.len()!=(end-offset) as usize {return Err(Error::OutputUnavailable)}
            }
            tx.commit()?;
            Ok(OutputRange{bytes,offset,next_offset:end,eof:end==size,size})
        })
    }
    /// Unlinked captures may be discarded. Linked evidence remains immutable.
    /// Quota measures logical bytes; SQLite can reuse freed pages without shrinking its file.
    pub fn discard_output(&self, id: &str, user_id: &str, output_id: &str) -> Result<()> {
        self.db(|db| {
            let tx = db.transaction()?;
            read_session(&tx, id, user_id)?;
            let output = read_complete(&tx, id, output_id)?;
            if output.event_id.is_some() {
                return Err(Error::Conflict);
            }
            tx.execute(
                "DELETE FROM agent_output_chunks WHERE output_id=?1",
                [output_id],
            )?;
            tx.execute(
                "DELETE FROM agent_outputs WHERE id=?1 AND session_id=?2 AND event_id IS NULL",
                params![output_id, id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn append_captured_result(
        &self,
        id: &str,
        user_id: &str,
        revision: u64,
        request_id: &str,
        request: CapturedResult,
    ) -> Result<AppendReceipt> {
        if request.output_ids.is_empty() || request.output_ids.len() > 2 {
            return Err(Error::Invalid(
                "a captured result needs one or two outputs".into(),
            ));
        }
        let mut outputs = Vec::new();
        let mut text = String::new();
        for output_id in request.output_ids {
            let output = self.output(id, user_id, &output_id)?;
            if output.call_id != request.call_id {
                return Err(Error::ToolProtocol);
            }
            text.push_str(&format!(
                "[{}; outputId={}; bytes={}; previewTruncated={}; previewLossy={}]\n{}\n",
                output.reference.channel.as_str(),
                output.reference.id,
                output.reference.size,
                output.preview_truncated,
                output.preview_lossy,
                output.preview
            ));
            outputs.push(output.reference);
        }
        self.append(
            id,
            user_id,
            revision,
            request_id,
            Payload::ToolResult {
                call_id: request.call_id,
                outcome: request.outcome,
                exit_code: request.exit_code,
                text,
                outputs,
            },
        )
    }
}
type OutputRow = (
    String,
    String,
    String,
    u64,
    String,
    String,
    bool,
    bool,
    Option<u64>,
);
pub(super) fn read_complete(db: &Connection, session_id: &str, id: &str) -> Result<StoredOutput> {
    let row: Option<OutputRow>=db.query_row(
        "SELECT task_id,call_id,channel,size,sha256,preview,preview_truncated,preview_lossy,event_id FROM agent_outputs WHERE id=?1 AND session_id=?2 AND state='complete'",
        params![id,session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional()?;
    let (
        task_id,
        call_id,
        channel,
        size,
        sha256,
        preview,
        preview_truncated,
        preview_lossy,
        event_id,
    ) = row.ok_or(Error::NotFound)?;
    Ok(StoredOutput {
        reference: OutputReference {
            id: id.into(),
            channel: OutputChannel::parse(&channel)?,
            size,
            sha256,
        },
        session_id: session_id.into(),
        task_id,
        call_id,
        preview,
        preview_truncated,
        preview_lossy,
        event_id,
    })
}
pub(super) fn validate_references(
    db: &Connection,
    session: &str,
    call: &str,
    refs: &[OutputReference],
) -> Result<()> {
    let mut channels = HashSet::new();
    for reference in refs {
        let output = read_complete(db, session, &reference.id)?;
        if output.reference != *reference
            || output.call_id != call
            || output.event_id.is_some()
            || !channels.insert(reference.channel.as_str())
        {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}
pub(super) fn link_references(db: &Connection, event: &Event) -> Result<()> {
    if let Payload::ToolResult { outputs, .. } = &event.payload {
        for output in outputs {
            if db.execute("UPDATE agent_outputs SET event_id=?1 WHERE id=?2 AND session_id=?3 AND event_id IS NULL AND state='complete'",
                params![event.id,output.id,event.session_id])?!=1 {return Err(Error::Conflict)}
        }
    }
    Ok(())
}
fn recover_expired(db: &Connection) -> Result<usize> {
    let now = chrono::Utc::now().timestamp_millis();
    db.execute("DELETE FROM agent_output_chunks WHERE output_id IN (SELECT id FROM agent_outputs WHERE state='writing' AND expires_at<=?1)",[now])?;
    Ok(db.execute(
        "DELETE FROM agent_outputs WHERE state='writing' AND expires_at<=?1",
        [now],
    )?)
}
