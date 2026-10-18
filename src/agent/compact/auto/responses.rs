use super::*;
use crate::agent::provider::{responses as protocol, ModelClient};

impl Controller {
    pub(super) async fn prepare_responses(
        &self,
        context: Context,
        request: &Request,
        target: Option<Target>,
        cancel: Cancellation,
    ) -> Result<Prepared> {
        let model = ModelClient::new(context.runtime.clone())?;
        let initial = project(&context, request, &target, None)?;
        let preparing = model.prepare_responses(&initial, &cancel.local);
        let (original, before) = tokio::select! {biased;
            _=cancel.caller.cancelled()=>return Err(ModelError::Cancelled.into()),
            result=preparing=>result?,
        };
        cancel.check()?;
        let available = context
            .runtime
            .config
            .budget()
            .available_input()
            .map_err(|_| Error::CompactionLimit)?;
        let fits = before <= available;
        self.reporter
            .measured(&context.runtime.config, before, Source::ProviderCount)?;
        let trigger = u128::from(before) * 100
            >= u128::from(available) * u128::from(context.runtime.config.compact_percent);
        let force = self.force || context.events.len() >= 200;
        if !context.runtime.config.remote_compaction || (!trigger && !force) {
            model.require_fit(before)?;
            ensure_current(self.store.clone(), context.snapshot.clone()).await?;
            return Ok(Prepared {
                runtime: context.runtime,
                request: initial,
                revision: context.snapshot.session.revision,
                responses: Some(original),
            });
        }
        let Some(frontier) = exchange_ends(&context.events)?
            .last()
            .copied()
            .or_else(|| context.checkpoint.as_ref().map(|c| c.through_event_id))
        else {
            model.require_fit(before)?;
            ensure_current(self.store.clone(), context.snapshot.clone()).await?;
            return Ok(Prepared {
                runtime: context.runtime,
                request: initial,
                revision: context.snapshot.session.revision,
                responses: Some(original),
            });
        };
        let pinned = project(
            &context,
            request,
            &target,
            Some((context.snapshot.through_event_id, None)),
        )?;
        let pinned_body = protocol::body(&context.runtime.config, &pinned)?;
        let checking = model.count_body(&pinned_body, &cancel.local);
        let pinned_tokens = tokio::select! {biased;
            _=cancel.caller.cancelled()=>return Err(ModelError::Cancelled.into()),
            result=checking=>result?,
        };
        model.require_fit(pinned_tokens)?;
        cancel.check()?;
        // A standalone compact request covers the historical window, never the incoming request.
        // Fresh trusted instructions are outside the returned input window and never dropped.
        let mut source = protocol::body(&context.runtime.config, &initial)?;
        // Durable memory is reloaded separately, never fed to an opaque historical summary.
        source["instructions"] = initial
            .messages
            .iter()
            .filter_map(|message| match message {
                ChatMessage::System { content } if Some(content) != context.memory.as_ref() => {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        let inputs = source["input"].as_array_mut().ok_or(Error::Storage)?;
        let has_incoming = !request.message.is_empty() || !request.attachments.is_empty();
        if has_incoming {
            let latest = inputs.pop().ok_or(Error::Storage)?;
            if latest
                != protocol::body(&context.runtime.config, &pinned)?["input"]
                    .as_array()
                    .and_then(|v| v.last())
                    .cloned()
                    .ok_or(Error::Storage)?
            {
                return Err(Error::Storage);
            }
        }
        if inputs.is_empty() {
            model.require_fit(before)?;
            return Ok(Prepared {
                runtime: context.runtime,
                request: initial,
                revision: context.snapshot.session.revision,
                responses: Some(original),
            });
        }
        self.reporter.emit(Update::Started(Method::Remote));
        let compacting = model.compact_responses(&source, &cancel.local);
        let candidate = tokio::select! {biased;
            _=cancel.caller.cancelled()=>return Err(ModelError::Cancelled.into()),
            result=compacting=>result,
        };
        let window = match candidate {
            Ok(window) => window,
            Err(error) => {
                cancel.check()?;
                ensure_current(self.store.clone(), context.snapshot.clone()).await?;
                if !fits {
                    self.reporter.emit(Update::Failed {
                        code: Error::from(error.clone()).code().into(),
                        fallback: false,
                    });
                    return Err(error.into());
                }
                self.reporter.emit(Update::Failed {
                    code: Error::from(error).code().into(),
                    fallback: true,
                });
                return Ok(Prepared {
                    runtime: context.runtime,
                    request: initial,
                    revision: context.snapshot.session.revision,
                    responses: Some(original),
                });
            }
        };
        cancel.check()?;
        let mut next = ChatRequest {
            messages: vec![],
            tools: vec![],
        };
        // Rebuild host-owned context after the network operation, never take authority from its window.
        self.check_live(request, &target).await?;
        let store = self.store.clone();
        let r = request.clone();
        let t = target.clone();
        let frozen = context.runtime.clone();
        let source = self.source.clone();
        let fresh = worker(move || load(store, &r, t, Some(frozen), source)).await?;
        if fresh.snapshot != context.snapshot {
            return Err(Error::ContextChanged);
        }
        let reinjected = project(
            &fresh,
            request,
            &target,
            Some((fresh.snapshot.through_event_id, None)),
        )?;
        next.messages.extend(
            reinjected
                .messages
                .iter()
                .filter(|m| {
                    matches!(
                        m,
                        ChatMessage::System { .. } | ChatMessage::Developer { .. }
                    )
                })
                .cloned(),
        );
        next.messages.push(ChatMessage::Responses {
            window: window.clone(),
        });
        if has_incoming {
            next.messages
                .push(reinjected.messages.last().cloned().ok_or(Error::Storage)?);
        }
        let operation = model.prepare_responses(&next, &cancel.local);
        let result = tokio::select! {biased;
            _=cancel.caller.cancelled()=>return Err(ModelError::Cancelled.into()),
            result=operation=>result,
        };
        let lower =
            available.saturating_mul(u64::from(context.runtime.config.compact_percent) * 8) / 1000;
        let prepared = match result {
            Ok((prepared, after)) if after <= lower && after < before => prepared,
            Ok(_) | Err(_) if fits => {
                cancel.check()?;
                ensure_current(self.store.clone(), context.snapshot.clone()).await?;
                self.reporter.emit(Update::Failed {
                    code: "AGENT_COMPACTION_LIMIT".into(),
                    fallback: true,
                });
                return Ok(Prepared {
                    runtime: context.runtime,
                    request: initial,
                    revision: context.snapshot.session.revision,
                    responses: Some(original),
                });
            }
            Ok(_) => return Err(Error::CompactionLimit),
            Err(error) => return Err(error.into()),
        };
        cancel.check()?;
        let mut evidence = context
            .checkpoint
            .as_ref()
            .map_or_else(Vec::new, |c| c.content.evidence_event_ids.clone());
        // Keep explicit failure references; raw logs remain independently available for every event.
        evidence.extend(
            context
                .unfinished
                .iter()
                .filter(|id| **id <= frontier)
                .copied(),
        );
        if evidence.is_empty() {
            evidence.push(frontier);
        }
        evidence.sort_unstable();
        evidence.dedup();
        let content = CheckpointContent {
            task: context.snapshot.session.task.clone(),
            summary: "Responses 远端整理窗口已保存；不透明内容仅由原供应商解释，原始历史仍保留。"
                .into(),
            next_steps: vec![],
            evidence_event_ids: evidence,
            retained_user_facts: context
                .checkpoint
                .as_ref()
                .map_or_else(Vec::new, |c| c.content.retained_user_facts.clone()),
            pending_tool_call_ids: context.snapshot.pending_tool_call_ids.clone(),
            uncertain_tool_call_ids: context.snapshot.uncertain_tool_call_ids.clone(),
        };
        let repo = Repository::new(self.store.clone());
        let snap = context.snapshot.clone();
        self.check_live(request, &target).await?;
        let stop = cancel.clone();
        let reporter = self.reporter.clone();
        let committed = worker(move || {
            stop.check()?;
            let result = repo.save_checkpoint_guarded_remote(
                &snap,
                frontier,
                content,
                Some(&window),
                || stop.cancelled(),
            );
            if let Ok(checkpoint) = &result {
                reporter.emit(Update::Committed(checkpoint.id.clone()));
            }
            stop.check()?;
            result.map_err(|error| match error {
                crate::agent::Error::Stale => Error::ContextChanged,
                other => Error::from(other),
            })
        })
        .await?;
        cancel.check()?;
        // Admission still checks this revision. No provider window can overwrite a newer message/target.
        let store = self.store.clone();
        let r = request.clone();
        let t = target.clone();
        let frozen = context.runtime.clone();
        let source = self.source.clone();
        let current = worker(move || load(store, &r, t, Some(frozen), source)).await?;
        if current.snapshot.session.revision != committed.committed_revision {
            return Err(Error::ContextChanged);
        }
        let rebuilt = project(&current, request, &target, None)?;
        let prepared = if protocol::body(&current.runtime.config, &rebuilt)?
            != protocol::body(&context.runtime.config, &next)?
        {
            let operation = model.prepare_responses(&rebuilt, &cancel.local);
            let (prepared, tokens) = tokio::select! {biased;_=cancel.caller.cancelled()=>return Err(ModelError::Cancelled.into()),result=operation=>result?};
            model.require_fit(tokens)?;
            prepared
        } else {
            prepared
        };
        Ok(Prepared {
            runtime: context.runtime,
            request: rebuilt,
            revision: committed.committed_revision,
            responses: Some(prepared),
        })
    }
}
