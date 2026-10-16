use axum::{
    extract::{OriginalUri, State},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        chat::{Attachment, Error, Manager, Request},
        provider::*,
        Repository,
    },
    files,
    store::Store,
};
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
#[derive(Clone, Default)]
struct Mock {
    bodies: Arc<Mutex<Vec<Value>>>,
}
async fn model(
    State(mock): State<Mock>,
    OriginalUri(uri): OriginalUri,
    Json(body): Json<Value>,
) -> axum::response::Response {
    mock.bodies.lock().unwrap().push(body.clone());
    if uri.path().ends_with("input_tokens") {
        return axum::response::IntoResponse::into_response(Json(
            json!({"object":"response.input_tokens","input_tokens":200}),
        ));
    }
    if uri.path().ends_with("responses") {
        let item = json!({"id":"message","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"已读取附件","annotations":[]}]});
        let events = vec![
            json!({"type":"response.created","response":{"id":"response","status":"in_progress"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":"message","type":"message","role":"assistant","status":"in_progress","content":[]}}),
            json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"item_id":"message","delta":"已读取附件"}),
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
            json!({"type":"response.completed","response":{"id":"response","object":"response","status":"completed","output":[item],"usage":{"input_tokens":200,"output_tokens":10,"total_tokens":210}}}),
        ];
        let sse = events
            .into_iter()
            .enumerate()
            .map(|(i, mut e)| {
                e["sequence_number"] = json!(i);
                format!("data: {e}\n\n")
            })
            .collect::<String>();
        return axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }
    if body["messages"][0]["content"]
        .as_str()
        .is_some_and(|s| s.contains("You summarize ShellPilot"))
    {
        let source: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let summary=json!({"summary":"已分析附件 evidence.txt，保留附件来源，尚无执行授权。","nextSteps":[],"evidenceEventIds":source["availableEvidenceEventIds"],"retainedUserFacts":[]}).to_string();
        return axum::response::IntoResponse::into_response(Json(
            json!({"choices":[{"index":0,"message":{"role":"assistant","content":summary},"finish_reason":"stop"}]}),
        ));
    }
    let events = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"已读取附件"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
    );
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(events))
        .unwrap()
}
struct Fixture {
    dir: tempfile::TempDir,
    store: Arc<Store>,
    files: Arc<files::Manager>,
    manager: Manager,
    mock: Mock,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(vision: bool, window: u64, responses: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("db.sqlite")).unwrap());
        let files = files::Manager::new_native(
            store.clone(),
            files::Settings::default(),
            uuid::Uuid::new_v4(),
        );
        let manager =
            Manager::new(store.clone(), CancellationToken::new()).with_files(files.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/", listener.local_addr().unwrap());
        let mock = Mock::default();
        let router = Router::new()
            .route("/chat/completions", post(model))
            .route("/responses", post(model))
            .route("/responses/input_tokens", post(model))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        ModelRepository::new(store.clone())
            .save(
                "model",
                "default",
                0,
                ModelConfigUpdate {
                    config: ModelConfig {
                        provider: if responses {
                            ProviderKind::OpenAiResponses
                        } else {
                            ProviderKind::OpenAiCompatible
                        },
                        remote_compaction: false,
                        base_url,
                        completions_path: if responses {
                            "responses"
                        } else {
                            "chat/completions"
                        }
                        .into(),
                        model: "fixture".into(),
                        context_window: window,
                        output_reserve: 512,
                        safety_reserve: 256,
                        compact_percent: 90,
                        output_token_parameter: OutputTokenParameter::MaxTokens,
                        capabilities: ModelCapabilities {
                            vision,
                            ..Default::default()
                        },
                        temperature: None,
                        reasoning_effort: None,
                        timeout_seconds: 10,
                        max_response_bytes: 65536,
                        require_api_key: false,
                    },
                    api_key: KeyChange::Keep,
                },
            )
            .unwrap();
        Self {
            dir,
            store,
            files,
            manager,
            mock,
            server,
        }
    }
    async fn upload(&self, name: &str, bytes: Vec<u8>) -> String {
        self.files
            .clone()
            .upload_reader(name.into(), String::new(), Cursor::new(bytes))
            .await
            .unwrap()
            .file_id
    }
    async fn request(&self, message: &str, ids: Vec<String>) -> Request {
        Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "model".into(),
            user_id: "default".into(),
            session_id: self
                .manager
                .create_session("model".into(), "default".into())
                .await
                .unwrap(),
            terminal_session_id: String::new(),
            message: message.into(),
            attachments: ids
                .into_iter()
                .map(|file_id| Attachment { file_id })
                .collect(),
        }
    }
    async fn send(&self, r: Request) -> std::result::Result<(), Error> {
        self.manager
            .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await
    }
    fn bodies(&self) -> Vec<Value> {
        self.mock.bodies.lock().unwrap().clone()
    }
}
fn image() -> Vec<u8> {
    let mut bytes = Cursor::new(vec![]);
    image::DynamicImage::new_rgb8(1800, 900)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}
fn pdf(text: &str) -> Vec<u8> {
    use lopdf::{dictionary, Object, Stream};
    let mut doc = lopdf::Document::with_version("1.5");
    let pages = doc.new_object_id();
    let font =
        doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resources = doc.add_object(dictionary! {"Font"=>dictionary!{"F1"=>font}});
    let content = lopdf::content::Content {
        operations: vec![
            lopdf::content::Operation::new("BT", vec![]),
            lopdf::content::Operation::new("Tf", vec!["F1".into(), 12.into()]),
            lopdf::content::Operation::new("Tj", vec![Object::string_literal(text)]),
            lopdf::content::Operation::new("ET", vec![]),
        ],
    }
    .encode()
    .unwrap();
    let stream = doc.add_object(Stream::new(dictionary! {}, content));
    let page=doc.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"Contents"=>stream,"Resources"=>resources,"MediaBox"=>vec![0.into(),0.into(),300.into(),300.into()]});
    doc.objects.insert(
        pages,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>vec![page.into()],"Count"=>1}),
    );
    let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    doc.trailer.set("Root", catalog);
    let mut bytes = vec![];
    doc.save_to(&mut bytes).unwrap();
    bytes
}
#[tokio::test]
async fn text_pdf_and_image_reach_the_model_with_sources_and_history_survives_original_cleanup() {
    let f = Fixture::new(true, 100_000, false).await;
    let ids = vec![
        f.upload("evidence.txt", b"deployment=blue; never reboot".to_vec())
            .await,
        f.upload("manual.pdf", pdf("PDF-42 deployment manual"))
            .await,
        f.upload("screen.png", image()).await,
    ];
    let r = f.request("分析这些附件", ids.clone()).await;
    f.send(r.clone()).await.unwrap();
    let body = &f.bodies()[0];
    let wire = body.to_string();
    assert!(wire.contains("deployment=blue"));
    assert!(wire.contains("PDF-42"));
    assert!(wire.contains("第 1 页"));
    assert!(wire.contains("data:image/jpeg;base64,"));
    assert!(!wire.contains("ipc://"));
    let history = f
        .manager
        .history(
            "model".into(),
            "default".into(),
            r.session_id.clone(),
            0,
            20,
        )
        .await
        .unwrap();
    assert_eq!(history.turns[0].attachments.len(), 3);
    assert_eq!(history.turns[0].attachments[2].width, Some(1024));
    assert_eq!(history.turns[0].attachments[2].height, Some(512));
    for id in &ids {
        f.files.delete(id).await.unwrap();
    }
    let preview = f
        .manager
        .attachment(
            "model".into(),
            "default".into(),
            r.session_id.clone(),
            ids[1].clone(),
        )
        .await
        .unwrap();
    let view = serde_json::to_value(preview).unwrap();
    assert!(view["text"].as_str().unwrap().contains("PDF-42"));
    f.send(r.clone()).await.unwrap();
    assert_eq!(f.bodies().len(), 1);
    let mut conflict = r.clone();
    conflict.attachments.clear();
    assert!(matches!(f.send(conflict).await, Err(Error::Conflict)));
    let reopened = Arc::new(Store::open(&f.dir.path().join("db.sqlite")).unwrap());
    let manager = Manager::new(reopened, CancellationToken::new());
    let mut next = r.clone();
    next.request_id = uuid::Uuid::new_v4().to_string();
    next.attachments.clear();
    next.message = "继续解释附件".into();
    manager
        .chat(next, None, &CancellationToken::new(), |_| async { Ok(()) })
        .await
        .unwrap();
    assert!(f.bodies()[1].to_string().contains("PDF-42"));
}
#[tokio::test]
async fn legacy_vision_opt_out_still_sends_images_in_both_protocols() {
    for responses in [false, true] {
        let f = Fixture::new(false, 100_000, responses).await;
        let id = f.upload("image.png", image()).await;
        let request = f.request("分析", vec![id]).await;
        f.send(request).await.unwrap();
        let bodies = f.bodies();
        let expected = if responses {
            "input_image"
        } else {
            "image_url"
        };
        assert!(bodies.last().unwrap().to_string().contains(expected));
        assert!(bodies
            .last()
            .unwrap()
            .to_string()
            .contains("data:image/jpeg;base64,"));
    }
}
#[tokio::test]
async fn images_require_media_budget_before_admission() {
    for vision in [false, true] {
        let window = 4096;
        let f = Fixture::new(vision, window, false).await;
        let id = f.upload("image.png", image()).await;
        let r = f.request("分析", vec![id]).await;
        let error = f.send(r.clone()).await.unwrap_err();
        assert!(
            matches!(error, Error::Model(ModelError::BudgetExceeded)),
            "{error:?}"
        );
        assert!(f.bodies().is_empty());
        assert!(f
            .manager
            .history("model".into(), "default".into(), r.session_id, 0, 20)
            .await
            .unwrap()
            .turns
            .is_empty());
    }
}
#[tokio::test]
async fn malformed_unsupported_empty_pdf_and_non_utf8_inputs_never_contact_model() {
    let f = Fixture::new(true, 100_000, false).await;
    for (name, bytes) in [
        ("bad.png", b"not an image".to_vec()),
        ("bad.txt", vec![0xff, 0xfe, 0]),
        ("bad.pdf", b"%PDF-broken".to_vec()),
        ("scan.pdf", pdf("")),
        ("large.txt", vec![b'a'; 262145]),
        ("data.docx", b"office".to_vec()),
    ] {
        let id = f.upload(name, bytes).await;
        let r = f.request("查看", vec![id]).await;
        assert!(
            matches!(f.send(r).await, Err(Error::Attachment(_))),
            "{name}"
        );
    }
    assert!(f.bodies().is_empty());
}
#[tokio::test]
async fn attachment_only_input_and_duplicate_ids_have_explicit_behavior() {
    let f = Fixture::new(false, 100_000, false).await;
    let id = f
        .upload("only.txt", b"only attachment input".to_vec())
        .await;
    let mut r = f.request("", vec![id.clone()]).await;
    f.send(r.clone()).await.unwrap();
    assert!(f.bodies()[0].to_string().contains("only attachment input"));
    r.request_id = uuid::Uuid::new_v4().to_string();
    r.attachments.push(Attachment { file_id: id });
    assert!(matches!(f.send(r).await, Err(Error::Attachment(_))));
    assert_eq!(f.bodies().len(), 1);
}
#[tokio::test]
async fn attachment_previews_and_references_are_scoped_to_the_exact_user_model_and_session() {
    let f = Fixture::new(false, 100_000, false).await;
    let id = f
        .upload("private.txt", b"private attachment".to_vec())
        .await;
    let r = f.request("查看", vec![id.clone()]).await;
    f.send(r.clone()).await.unwrap();
    let other = f.request("other", vec![]).await;
    for (agent, user, session) in [
        ("wrong", "default", r.session_id.clone()),
        ("model", "stranger", r.session_id.clone()),
        ("model", "default", other.session_id),
    ] {
        assert!(f
            .manager
            .attachment(agent.into(), user.into(), session, id.clone())
            .await
            .is_err());
    }
    let other_session = Repository::new(f.store.clone())
        .create_session("other-model", "stranger")
        .unwrap();
    let profile = ModelRepository::new(f.store.clone())
        .get("model", "default")
        .unwrap();
    ModelRepository::new(f.store.clone())
        .save(
            "other-model",
            "stranger",
            0,
            ModelConfigUpdate {
                config: profile.config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    let mut attack = r;
    attack.agent_id = "other-model".into();
    attack.user_id = "stranger".into();
    attack.session_id = other_session.id;
    attack.request_id = uuid::Uuid::new_v4().to_string();
    assert!(matches!(f.send(attack).await, Err(Error::Attachment(_))));
    assert_eq!(f.bodies().len(), 1);
}
#[tokio::test]
async fn responses_sends_real_image_parts_to_both_count_and_generation() {
    let f = Fixture::new(true, 100_000, true).await;
    let id = f.upload("image.png", image()).await;
    let r = f.request("看图", vec![id]).await;
    f.send(r).await.unwrap();
    let bodies = f.bodies();
    assert!(bodies.len() >= 2);
    assert!(bodies
        .iter()
        .all(|b| b["input"].to_string().contains("input_image")));
    assert!(bodies
        .iter()
        .all(|b| b["input"].to_string().contains("data:image/jpeg;base64,")));
}
#[tokio::test]
async fn compaction_receives_attachment_content_without_executable_tools_and_keeps_raw_preview() {
    let f = Fixture::new(false, 100_000, false).await;
    let id = f
        .upload("evidence.txt", b"EXACT-PUBLIC-ID-42".to_vec())
        .await;
    let r = f.request("分析", vec![id.clone()]).await;
    f.send(r.clone()).await.unwrap();
    let summary = shellpilot_server::agent::compact::Summarizer::new(f.store.clone())
        .summarize(&r.session_id, "default", &CancellationToken::new())
        .await
        .unwrap();
    assert!(summary.content.summary.contains("evidence.txt"));
    let body = f.bodies().last().unwrap().clone();
    assert!(body.get("tools").is_none());
    assert!(body.to_string().contains("EXACT-PUBLIC-ID-42"));
    assert!(body.to_string().contains("原始用户事件"));
    Repository::new(f.store.clone())
        .save_checkpoint(&summary.snapshot, summary.content)
        .unwrap();
    assert!(f
        .manager
        .attachment("model".into(), "default".into(), r.session_id, id)
        .await
        .is_ok());
}

#[tokio::test]
async fn removing_a_conversation_cleans_attachment_snapshots_without_deleting_other_uploads() {
    let f = Fixture::new(false, 100_000, false).await;
    let id = f
        .upload("retain.txt", b"retain original upload".to_vec())
        .await;
    let r = f.request("analyse", vec![id.clone()]).await;
    f.send(r.clone()).await.unwrap();
    f.manager
        .delete_chat("model".into(), "default".into(), r.session_id.clone())
        .await
        .unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("db.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM agent_attachments", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM agent_event_attachments", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(f.files.metadata(&id).await.is_ok());
}
#[tokio::test]
async fn schema_sixteen_upgrade_preserves_upload_owner_and_text_history() {
    let f = Fixture::new(false, 100_000, false).await;
    let id = f.upload("legacy.txt", b"legacy upload".to_vec()).await;
    let r = f.request("before migration", vec![]).await;
    f.send(r.clone()).await.unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("db.sqlite")).unwrap();
    db.execute_batch(
        "DROP TABLE agent_event_attachments;DROP TABLE agent_attachments;PRAGMA user_version=16",
    )
    .unwrap();
    drop(db);
    let store = Arc::new(Store::open(&f.dir.path().join("db.sqlite")).unwrap());
    let manager = Manager::new(store, CancellationToken::new()).with_files(f.files.clone());
    // A file manager from another Store handle is rejected, even when the filesystem path matches.
    let mut input = r.clone();
    input.request_id = uuid::Uuid::new_v4().to_string();
    input.attachments = vec![Attachment {
        file_id: id.clone(),
    }];
    assert!(matches!(
        manager
            .chat(input, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await,
        Err(Error::Storage)
    ));
    let db = rusqlite::Connection::open(f.dir.path().join("db.sqlite")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        db.query_row("SELECT owner_id FROM file_assets WHERE id=?1", [id], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "default"
    );
    assert_eq!(
        f.manager
            .history("model".into(), "default".into(), r.session_id, 0, 20)
            .await
            .unwrap()
            .turns
            .len(),
        1
    );
}

#[tokio::test]
async fn inline_image_encoding_length_does_not_masquerade_as_text_tokens() {
    use shellpilot_server::agent::attachments::{ImageUrl, Part, IMAGE_TOKENS};
    let f = Fixture::new(true, 100_000, false).await;
    let profile = ModelRepository::new(f.store.clone())
        .get("model", "default")
        .unwrap();
    let request = |n| ChatRequest {
        messages: vec![ChatMessage::UserMedia {
            content: vec![Part::ImageUrl {
                image_url: ImageUrl {
                    url: format!("data:image/jpeg;base64,{}", "A".repeat(n)),
                    detail: "high".into(),
                },
            }],
        }],
        tools: vec![],
    };
    let small = prepare_request(&profile.config, &request(100)).unwrap();
    let large = prepare_request(&profile.config, &request(200_000)).unwrap();
    assert_eq!(small.estimated_input_tokens, large.estimated_input_tokens);
    assert!(large.estimated_input_tokens >= IMAGE_TOKENS);
    assert!(large.body.len() > small.body.len() + 100_000);
    assert!(f.bodies().is_empty());
}

#[tokio::test]
async fn automatic_compaction_keeps_the_latest_image_atomic_and_the_original_file_readable() {
    let f = Fixture::new(true, 65_536, false).await;
    let models = ModelRepository::new(f.store.clone());
    let mut profile = models.get("model", "default").unwrap();
    profile.config.compact_percent = 25;
    models
        .save(
            "model",
            "default",
            profile.revision,
            ModelConfigUpdate {
                config: profile.config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    let text_id = f.upload("evidence.txt", vec![b'a'; 40_000]).await;
    let first = f.request("分析附件", vec![text_id.clone()]).await;
    f.send(first.clone()).await.unwrap();
    let image_id = f.upload("latest.png", image()).await;
    let mut latest = first.clone();
    latest.request_id = uuid::Uuid::new_v4().to_string();
    latest.message = "根据新图片继续".into();
    latest.attachments = vec![Attachment {
        file_id: image_id.clone(),
    }];
    f.send(latest).await.unwrap();
    let checkpoint = Repository::new(f.store.clone())
        .checkpoint(&first.session_id, "default")
        .unwrap()
        .unwrap();
    assert!(checkpoint.content.summary.contains("evidence.txt"));
    let bodies = f.bodies();
    assert_eq!(bodies.len(), 3);
    let final_body = bodies.last().unwrap().to_string();
    assert!(final_body.contains("data:image/jpeg;base64,"));
    assert!(final_body.contains(&image_id));
    assert!(!final_body.contains(&"a".repeat(40_000)));
    let original = f
        .manager
        .attachment("model".into(), "default".into(), first.session_id, text_id)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(original).unwrap()["text"]
            .as_str()
            .unwrap()
            .len(),
        40_000
    );
}
