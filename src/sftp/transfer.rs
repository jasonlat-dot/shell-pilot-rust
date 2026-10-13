use super::{wire::Wire, *};
use axum::{
    body::{Body, Bytes},
    http::{header, StatusCode},
    response::Response,
};
use futures_util::StreamExt;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode as Code};
use std::future::Future;

impl Manager {
    pub async fn create_transfer(&self, request: CreateRequest) -> Result<TransferView> {
        let session = self.session(&request.sftp_session_id)?;
        let _active = session.begin();
        wire::operation(&session, async {
            let _lock = session
                .metadata
                .acquire()
                .await
                .map_err(|_| Error::cancelled())?;
            if session.tasks.lock().unwrap().len() >= self.settings.max_tasks {
                return Err(Error::busy());
            }
            let wire = Wire::new(&session).await?;
            let selected = wire
                .checked(&session.view.root_path, &request.remote_path, false)
                .await?;
            let attrs = wire.stat(&selected).await?.ok_or_else(Error::missing)?;
            let (base, manifest) = if request.direction == Direction::Upload {
                if !attrs.is_dir() {
                    return Err(Error::invalid("上传目标必须是目录"));
                }
                (
                    selected.clone(),
                    path::manifest(
                        request
                            .items
                            .ok_or_else(|| Error::invalid("上传必须提供清单"))?,
                        self.settings.max_items,
                    )?,
                )
            } else {
                if request.items.as_ref().is_some_and(|i| !i.is_empty()) {
                    return Err(Error::invalid("下载清单由服务器生成"));
                }
                if selected == "/" {
                    return Err(Error::invalid(
                        "请选择远程文件或目录，不支持下载文件系统根目录",
                    ));
                }
                let mut pending = vec![selected.clone()];
                let mut manifest = Vec::new();
                let base = path::parent(&selected).to_string();
                while let Some(target) = pending.pop() {
                    let target = wire
                        .checked(&session.view.root_path, &target, false)
                        .await?;
                    let attrs = wire.stat(&target).await?.ok_or_else(Error::missing)?;
                    let entry = wire::entry(String::new(), target.clone(), &attrs);
                    if !matches!(entry.kind, Kind::File | Kind::Directory) {
                        return Err(Error::path("目录下载不跟随符号链接或特殊文件"));
                    }
                    let relative = target
                        .strip_prefix(&(base.trim_end_matches('/').to_string() + "/"))
                        .ok_or_else(|| Error::path("下载路径超出基准目录"))?
                        .to_string();
                    path::relative(&relative)?;
                    manifest.push(ManifestItem {
                        relative_path: relative,
                        kind: entry.kind,
                        size: entry.size,
                    });
                    if entry.kind == Kind::Directory {
                        for entry in wire
                            .list(
                                &target,
                                self.settings.max_items.saturating_sub(manifest.len()),
                            )
                            .await?
                        {
                            pending.push(entry.path);
                        }
                    }
                    if manifest.len() + pending.len() > self.settings.max_items {
                        return Err(Error::limit());
                    }
                }
                (base, path::manifest(manifest, self.settings.max_items)?)
            };
            let capacity = self
                .capacity
                .clone()
                .try_acquire_many_owned(manifest.len() as u32)
                .map_err(|_| Error::busy())?;
            let task = Arc::new(Task {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session.view.sftp_session_id.clone(),
                direction: request.direction,
                remote: selected,
                base,
                conflict: request.conflict,
                stop: session.stop.child_token(),
                data: Mutex::new(TaskData {
                    items: manifest.into_iter().map(Item::from).collect(),
                    updated_at: chrono::Utc::now(),
                    touched: Instant::now(),
                }),
                _capacity: capacity,
            });
            if task.direction == Direction::Download {
                task.mutate(|items| {
                    for item in items.iter_mut().filter(|i| i.kind == Kind::Directory) {
                        item.status = Status::Sent;
                    }
                });
            }
            if task.direction == Direction::Upload {
                // Parent directories precede their children. Failed entries stay visible and retryable.
                let items = task.view().items;
                for item in items {
                    let target = path::join(&task.base, &item.relative_path);
                    let result = async {
                        if item.kind == Kind::Directory {
                            wire.mkdirs(&session.view.root_path, &target).await?;
                            task.finish(&item.item_id, Status::Completed, None);
                        } else {
                            wire.checked(&session.view.root_path, &target, true).await?;
                            if let Some(attrs) = wire.stat(&target).await? {
                                if !attrs.is_regular() {
                                    return Err(Error::conflict("上传目标不是普通文件"));
                                }
                                match task.conflict {
                                    Conflict::Fail => {
                                        return Err(Error::conflict("远程文件已存在，不会覆盖"))
                                    }
                                    Conflict::Skip => {
                                        task.finish(&item.item_id, Status::Skipped, None)
                                    }
                                    Conflict::Replace => {
                                        if !wire.posix_rename {
                                            return Err(Error::new(
                                                "SFTP_UNSUPPORTED",
                                                "安全替换需要 posix-rename@openssh.com 扩展",
                                                StatusCode::BAD_GATEWAY,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        if item.kind == Kind::Directory {
                            return Err(error);
                        }
                        task.finish(&item.item_id, Status::Failed, Some(&error));
                    }
                }
            }
            let mut tasks = session.tasks.lock().unwrap();
            if session.stop.is_cancelled() {
                task.cancel();
                return Err(Error::cancelled());
            }
            let view = task.view();
            tasks.insert(task.id.clone(), task);
            Ok(view)
        })
        .await
    }
    pub fn confirm(&self, id: &str, item_id: &str, saved: bool) -> Result<()> {
        let (_session, task) = self.task(id)?;
        if task.direction != Direction::Download {
            return Err(Error::invalid("只能确认下载"));
        }
        let mut data = task.data.lock().unwrap();
        let item = data
            .items
            .iter_mut()
            .find(|i| i.item_id == item_id)
            .ok_or_else(Error::missing)?;
        if task.stop.is_cancelled() {
            return Err(Error::cancelled());
        }
        if item.status == Status::Completed {
            return Ok(());
        }
        if !saved && item.status != Status::Running {
            item.status = Status::Failed;
            item.error_code = Some("SFTP_LOCAL_SAVE_FAILED".into());
            item.message = Some("客户端未成功保存，请重新下载".into());
        } else if saved
            && (item.status == Status::Sent
                || (item.kind == Kind::Directory
                    && matches!(item.status, Status::Pending | Status::Failed)))
        {
            item.status = Status::Completed;
            item.error_code = None;
            item.message = None;
        } else {
            return Err(Error::conflict("下载仍未发送完成或条目不能确认"));
        }
        data.updated_at = chrono::Utc::now();
        data.touched = Instant::now();
        Ok(())
    }
    pub async fn upload(
        &self,
        id: &str,
        item_id: &str,
        body: Body,
        content_length: Option<u64>,
    ) -> Result<()> {
        let (session, task) = self.task(id)?;
        let item = task.item(item_id)?;
        if task.direction != Direction::Upload || item.kind != Kind::File {
            return Err(Error::invalid("条目不可上传"));
        }
        if content_length.is_some_and(|length| length != item.size) {
            return Err(Error::size());
        }
        let run = Running::claim(session.clone(), task.clone(), item_id, &self.transfers)?;
        let idle = self.settings.stream_idle_timeout;
        // The operation owns the HTTP body and item guard if its caller disappears.
        tokio::spawn(async move {
            let mut run = run;
            let result = upload(&session, &task, &item, body, idle).await;
            run.complete(&result, Status::Completed);
            result
        })
        .await
        .map_err(|_| Error::io("上传工作线程异常"))?
    }
    pub async fn download(&self, id: &str, item_id: &str) -> Result<Response> {
        let (session, task) = self.task(id)?;
        let item = task.item(item_id)?;
        if task.direction != Direction::Download || item.kind != Kind::File {
            return Err(Error::invalid("条目不可下载"));
        }
        let mut run = Running::claim(session.clone(), task.clone(), item_id, &self.transfers)?;
        let wire = wait(
            &task,
            self.settings.stream_idle_timeout,
            Wire::new(&session),
        )
        .await?;
        let target = path::join(&task.base, &item.relative_path);
        wait(&task, self.settings.stream_idle_timeout, async {
            wire.checked(&session.view.root_path, &target, false)
                .await?;
            let attrs = wire.stat(&target).await?.ok_or_else(Error::missing)?;
            if !attrs.is_regular() || attrs.size != Some(item.size) {
                return Err(Error::source());
            }
            Ok(())
        })
        .await?;
        let handle = wait(&task, self.settings.stream_idle_timeout, async {
            Ok(wire
                .raw
                .open(&target, OpenFlags::READ, FileAttributes::default())
                .await?
                .handle)
        })
        .await?;
        let idle = self.settings.stream_idle_timeout;
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Result<Bytes>>(2);
        tokio::spawn(async move {
            let result: Result<()> = async {
                let mut bytes = 0u64;
                loop {
                    let chunk = wait(&task, idle, async {
                        match wire.raw.read(&handle, bytes, 32768).await {
                            Ok(data) if !data.data.is_empty() => Ok(Some(data.data)),
                            Ok(_) => Err(Error::io("服务器返回空读取响应")),
                            Err(russh_sftp::client::error::Error::Status(s))
                                if s.status_code == Code::Eof =>
                            {
                                Ok(None)
                            }
                            Err(error) => Err(error.into()),
                        }
                    })
                    .await?;
                    let Some(chunk) = chunk else { break };
                    bytes = bytes
                        .checked_add(chunk.len() as u64)
                        .ok_or_else(Error::size)?;
                    if bytes > item.size {
                        return Err(Error::source());
                    }
                    task.progress(&item.item_id, bytes);
                    wait(&task, idle, async {
                        sender
                            .send(Ok(Bytes::from(chunk)))
                            .await
                            .map_err(|_| Error::io("客户端已中断下载"))
                    })
                    .await?;
                }
                wait(&task, idle, async {
                    if bytes != item.size {
                        return Err(Error::source());
                    }
                    let attrs = wire.raw.fstat(&handle).await?.attrs;
                    if attrs.size != Some(item.size) || !attrs.is_regular() {
                        return Err(Error::source());
                    }
                    wire.raw.close(&handle).await?;
                    Ok(())
                })
                .await?;
                Ok(())
            }
            .await;
            run.complete(&result, Status::Sent);
            if let Err(error) = result {
                let _ = tokio::time::timeout(idle, sender.send(Err(error))).await;
            }
        });
        let stream =
            async_stream::stream! {while let Some(chunk)=receiver.recv().await {yield chunk;}};
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CACHE_CONTROL, "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(Body::from_stream(stream))
            .expect("static response headers"))
    }
}
// Item state, per-session/global permits, and session activity form one ownership scope.
struct Running {
    session: Arc<Session>,
    task: Arc<Task>,
    id: String,
    _global: OwnedSemaphorePermit,
    _slot: OwnedSemaphorePermit,
    _active: Active,
    done: bool,
}
impl Running {
    fn claim(
        session: Arc<Session>,
        task: Arc<Task>,
        id: &str,
        global: &Arc<Semaphore>,
    ) -> Result<Self> {
        let slot = session
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let global = global
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let active = session.begin();
        let mut data = task.data.lock().unwrap();
        if task.stop.is_cancelled() {
            return Err(Error::cancelled());
        }
        let item = data
            .items
            .iter_mut()
            .find(|i| i.item_id == id)
            .ok_or_else(Error::missing)?;
        if !matches!(item.status, Status::Pending | Status::Failed) {
            return Err(Error::conflict("条目正在处理、等待确认或已完成"));
        }
        item.status = Status::Running;
        item.transferred_bytes = 0;
        item.error_code = None;
        item.message = None;
        data.updated_at = chrono::Utc::now();
        data.touched = Instant::now();
        drop(data);
        Ok(Self {
            session,
            task,
            id: id.into(),
            _global: global,
            _slot: slot,
            _active: active,
            done: false,
        })
    }
    fn complete(&mut self, result: &Result<()>, success: Status) {
        if let Err(error) = result {
            self.task.finish(&self.id, Status::Failed, Some(error));
        } else if self
            .task
            .item(&self.id)
            .is_ok_and(|i| i.status != Status::Skipped)
        {
            self.task.finish(&self.id, success, None);
        }
        self.done = true;
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.done {
            let error = if self.task.stop.is_cancelled() || self.session.stop.is_cancelled() {
                Error::cancelled()
            } else {
                Error::io("文件流已中断，请查询状态后重试")
            };
            self.task.finish(&self.id, Status::Failed, Some(&error));
        }
    }
}
async fn wait<T>(
    task: &Task,
    idle: Duration,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {biased;_=task.stop.cancelled()=>Err(Error::cancelled()),value=tokio::time::timeout(idle,future)=>value.map_err(|_|Error::timeout())?}
}
async fn upload(
    session: &Arc<Session>,
    task: &Arc<Task>,
    item: &Item,
    body: Body,
    idle: Duration,
) -> Result<()> {
    let wire = wait(task, idle, Wire::new(session)).await?;
    let target = path::join(&task.base, &item.relative_path);
    let temporary = path::join(
        path::parent(&target),
        &format!(".sftp-{}-{}.part", task.id, item.item_id),
    );
    let mut temporary_owned = false;
    let mut opened_handle: Option<String> = None;
    let result = async {
        wait(
            task,
            idle,
            wire.mkdirs(&session.view.root_path, path::parent(&target)),
        )
        .await?;
        wait(
            task,
            idle,
            wire.checked(&session.view.root_path, &target, true),
        )
        .await?;
        let original = wait(task, idle, wire.stat(&target)).await?;
        if let Some(attrs) = &original {
            if !attrs.is_regular() {
                return Err(Error::conflict("目标不是普通文件"));
            }
            match task.conflict {
                Conflict::Skip => {
                    task.finish(&item.item_id, Status::Skipped, None);
                    return Ok(());
                }
                Conflict::Fail => return Err(Error::conflict("远程文件已存在，不会覆盖")),
                Conflict::Replace => {
                    if !wire.posix_rename {
                        return Err(Error::new(
                            "SFTP_UNSUPPORTED",
                            "安全替换需要 posix-rename 扩展",
                            StatusCode::BAD_GATEWAY,
                        ));
                    }
                }
            }
        }
        if wait(task, idle, wire.stat(&temporary)).await?.is_some() {
            return Err(Error::conflict("临时文件已存在，请创建新任务"));
        }
        // OPEN may create the unique temporary file before cancellation drops its reply.
        // Retain cleanup responsibility before sending, including that ambiguous outcome.
        temporary_owned = true;
        let handle = wait(task, idle, async {
            Ok(wire
                .raw
                .open(
                    &temporary,
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                    FileAttributes {
                        permissions: Some(0o100600),
                        ..Default::default()
                    },
                )
                .await?
                .handle)
        })
        .await?;
        opened_handle = Some(handle.clone());
        let mut body = body.into_data_stream();
        let mut bytes = 0u64;
        loop {
            let chunk = wait(task, idle, async {
                body.next()
                    .await
                    .transpose()
                    .map_err(|_| Error::io("上传 HTTP 文件流中断"))
            })
            .await?;
            let Some(chunk) = chunk else { break };
            for block in chunk.chunks(32768) {
                let next = bytes
                    .checked_add(block.len() as u64)
                    .ok_or_else(Error::size)?;
                if next > item.size {
                    return Err(Error::size());
                }
                wait(task, idle, async {
                    wire.raw.write(&handle, bytes, block.to_vec()).await?;
                    Ok(())
                })
                .await?;
                bytes = next;
                task.progress(&item.item_id, bytes);
            }
        }
        if bytes != item.size {
            return Err(Error::size());
        }
        wait(task, idle, async {
            let attrs = wire.raw.fstat(&handle).await?.attrs;
            if attrs.size != Some(item.size) {
                return Err(Error::size());
            }
            wire.raw.close(&handle).await?;
            wire.checked(&session.view.root_path, &target, true).await?;
            let present = wire.stat(&target).await?;
            if let (Conflict::Replace, Some(present)) = (task.conflict, present.as_ref()) {
                if !present.is_regular() {
                    return Err(Error::conflict("目标已变化为其他文件类型"));
                }
                if let Some(original) = original {
                    if present.size != original.size || present.mtime != original.mtime {
                        return Err(Error::conflict("目标文件已变化，请重新创建任务"));
                    }
                }
                if !wire.posix_rename {
                    return Err(Error::new(
                        "SFTP_UNSUPPORTED",
                        "安全替换需要 posix-rename 扩展",
                        StatusCode::BAD_GATEWAY,
                    ));
                }
                let mut payload = Vec::new();
                for part in [&temporary, &target] {
                    payload.extend_from_slice(&(part.len() as u32).to_be_bytes());
                    payload.extend_from_slice(part.as_bytes());
                }
                match wire
                    .raw
                    .extended("posix-rename@openssh.com", payload)
                    .await?
                {
                    russh_sftp::protocol::Packet::Status(status)
                        if status.status_code == Code::Ok => {}
                    russh_sftp::protocol::Packet::Status(status) => {
                        return Err(russh_sftp::client::error::Error::Status(status).into())
                    }
                    _ => return Err(Error::io("服务器返回无效的替换响应")),
                }
            } else {
                if present.is_some() {
                    return Err(Error::conflict("提交时发现同名文件，不会覆盖"));
                }
                if wire.hardlink {
                    wire.raw.hardlink(&temporary, &target).await?;
                } else {
                    wire.raw.rename(&temporary, &target).await?;
                } // SFTP v3 RENAME must reject an existing destination.
            }
            Ok(())
        })
        .await?;
        Ok(())
    }
    .await;
    if let Some(handle) = opened_handle {
        let _ = tokio::time::timeout(Duration::from_secs(5), wire.raw.close(handle)).await;
    }
    if temporary_owned {
        // Cleanup uses a short independent budget so cancellation still removes a partial upload.
        let cleanup =
            tokio::time::timeout(Duration::from_secs(5), wire.raw.remove(&temporary)).await;
        if !matches!(
            cleanup,
            Ok(Ok(_))
                | Ok(Err(russh_sftp::client::error::Error::Status(
                    russh_sftp::protocol::Status {
                        status_code: Code::NoSuchFile,
                        ..
                    }
                )))
        ) {
            tracing::warn!(path=%temporary,"SFTP temporary upload cleanup not confirmed");
        }
    }
    result
}
