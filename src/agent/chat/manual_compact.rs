//! Explicit idle-session compaction, without admitting a turn or executing tools.
use super::*;
use crate::agent::context_status::{self, Observation, Phase, Reporter, Source, Update};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    pub compacted: bool,
    pub status: context_status::Status,
}

impl Manager {
    pub async fn compact_context(
        &self,
        agent: String,
        user: String,
        session: String,
        cancel: &CancellationToken,
    ) -> Result<CompactionResult> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled.into());
        }
        let store = self.store.clone();
        let (a, u, s) = (agent.clone(), user.clone(), session.clone());
        let target = worker(move || {
            let repo = Repository::new(store);
            let snapshot = repo.snapshot(&s, &u)?;
            if snapshot.session.agent_id != a { return Err(Error::NotFound); }
            let running = repo.db(|db| Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_chat_turns WHERE session_id=?1 AND state='running')",
                [&s], |row| row.get::<_, bool>(0),
            )?))?;
            if running || !snapshot.pending_tool_call_ids.is_empty() { return Err(Error::Busy); }
            Ok(snapshot.session.task.and_then(|task| task.target))
        }).await?;
        let request = Request {
            request_id: new_request_id(),
            agent_id: agent.clone(),
            user_id: user.clone(),
            session_id: session.clone(),
            terminal_session_id: String::new(),
            message: String::new(),
            attachments: vec![],
        };
        let token = self.stop.child_token();
        let key = (user.clone(), session.clone());
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(&key) || active.len() >= 32 {
                return Err(Error::Busy);
            }
            active.insert(
                key.clone(),
                Active {
                    request: request.request_id.clone(),
                    token: token.clone(),
                    started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                },
            );
        }
        // finished=true means dropping this guard never writes a chat turn.
        let mut guard = Running {
            manager: self,
            key,
            token: token.clone(),
            heartbeat: CancellationToken::new(),
            request: request.clone(),
            owner: String::new(),
            partial: Arc::new(Mutex::new(String::new())),
            finished: true,
            observation: None,
        };
        let store = self.store.clone();
        let r = request.clone();
        let observation = worker(move || Observation::begin(store, &r)).await?;
        guard.observation = Some(observation.clone());
        let (reporter, mut updates) = Reporter::channel(observation.clone());
        let controller = crate::agent::compact::auto::Controller::with_source(
            self.store.clone(),
            self.live.clone(),
        )
        .with_reporter(reporter.clone())
        .force();
        let operation = async {
            let prepared = controller
                .prepare_text(&request, target, &token, cancel)
                .await?;
            let (tokens, source) = if let Some(responses) = &prepared.responses {
                (responses.input_tokens, responses.input_source.clone())
            } else {
                (
                    crate::agent::provider::measure_request(
                        &prepared.runtime.config,
                        &prepared.request,
                    )?
                    .estimated_input_tokens,
                    Source::Estimated,
                )
            };
            reporter.measured(&prepared.runtime.config, tokens, source)?;
            Ok::<_, Error>(())
        };
        tokio::pin!(operation);
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        let result = loop {
            tokio::select! { biased;
                Some(update)=updates.recv()=>{let observed=observation.clone();worker(move||observed.apply(update).map(|_|())).await?;},
                result=&mut operation=>break result,
                _=tick.tick()=>{let observed=observation.clone();worker(move||observed.heartbeat()).await?;},
            }
        };
        while let Ok(update) = updates.try_recv() {
            let observed = observation.clone();
            worker(move || observed.apply(update).map(|_| ())).await?;
        }
        let failure = result.as_ref().err().map(|error| error.code().to_owned());
        let observed = observation.clone();
        worker(move || observed.apply(Update::Finished(failure)).map(|_| ())).await?;
        guard.observation = None;
        result?;
        let status = self.context_status(agent, user, session).await?;
        Ok(CompactionResult {
            compacted: matches!(status.phase, Phase::Succeeded),
            status,
        })
    }
}
