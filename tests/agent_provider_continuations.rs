//! Loopback-only protocol contracts. No vendor, SSH server or user data is used.
use axum::{
    body::Body, extract::State, http::Uri, response::Response, routing::post, Json, Router,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        approvals::{Approvals, Policy},
        provider::*,
        tool_loop::*,
        Repository,
    },
    store::Store,
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Wire {
    replies: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    protocol: ProviderKind,
}
async fn respond(State(w): State<Wire>, uri: Uri, Json(body): Json<Value>) -> Response {
    if uri.path().ends_with("input_tokens") {
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"object":"response.input_tokens","input_tokens":100}).to_string(),
            ))
            .unwrap();
    }
    w.requests.lock().unwrap().push(body.clone());
    let reply = w
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected retry");
    let stream = body["stream"] == true;
    let bytes = if stream {
        sse(&w.protocol, &reply)
    } else {
        reply.to_string()
    };
    // Split across UTF-8 characters and SSE frames as real transports can.
    let chunks = bytes
        .into_bytes()
        .chunks(7)
        .map(|b| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(b)))
        .collect::<Vec<_>>();
    Response::builder()
        .header(
            "content-type",
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap()
}
fn sse(protocol: &ProviderKind, reply: &Value) -> String {
    let mut events = vec![];
    match protocol {
        ProviderKind::OpenAiCompatible => {
            let message = &reply["choices"][0]["message"];
            if let Some(reasoning) = message["reasoning_content"].as_str() {
                for part in ["", reasoning] {
                    events.push(json!({"choices":[{"index":0,"delta":{"reasoning_content":part},"finish_reason":null}]}));
                }
            }
            let mut delta = message.clone();
            delta.as_object_mut().unwrap().remove("reasoning_content");
            if let Some(calls) = delta["tool_calls"].as_array_mut() {
                for (i, c) in calls.iter_mut().enumerate() {
                    c["index"] = i.into();
                }
            }
            events.push(json!({"choices":[{"index":0,"delta":delta,"finish_reason":reply["choices"][0]["finish_reason"]}]}));
        }
        ProviderKind::AnthropicMessages => {
            events.push(json!({"type":"message_start","message":{"type":"message","role":"assistant","content":[]}}));
            for (i, block) in reply["content"].as_array().unwrap().iter().enumerate() {
                let mut start = block.clone();
                let thinking = start.as_object_mut().unwrap().remove("thinking");
                let signature = start.as_object_mut().unwrap().remove("signature");
                if thinking.is_some() {
                    start["thinking"] = "".into();
                }
                events.push(json!({"type":"content_block_start","index":i,"content_block":start}));
                if let Some(thinking) = thinking {
                    events.push(json!({"type":"content_block_delta","index":i,"delta":{"type":"thinking_delta","thinking":thinking}}));
                }
                if let Some(signature) = signature {
                    for part in signature.as_str().unwrap().as_bytes().chunks(3) {
                        events.push(json!({"type":"content_block_delta","index":i,"delta":{"type":"signature_delta","signature":std::str::from_utf8(part).unwrap()}}));
                    }
                }
                events.push(json!({"type":"content_block_stop","index":i}));
            }
            events
                .push(json!({"type":"message_delta","delta":{"stop_reason":reply["stop_reason"]}}));
            events.push(json!({"type":"message_stop"}));
        }
        ProviderKind::OpenAiResponses => {
            events.push(json!({"type":"response.created","response":{"id":reply["id"]}}));
            for (i, item) in reply["output"].as_array().unwrap().iter().enumerate() {
                events.push(
                    json!({"type":"response.output_item.added","output_index":i,"item":item}),
                );
                if item["type"] == "message" {
                    for (j, part) in item["content"].as_array().unwrap().iter().enumerate() {
                        events.push(json!({"type":"response.output_text.delta","output_index":i,"item_id":item["id"],"content_index":j,"delta":part["text"]}));
                    }
                } else if item["type"] == "function_call" {
                    events.push(json!({"type":"response.function_call_arguments.delta","output_index":i,"item_id":item["id"],"delta":item["arguments"]}));
                    events.push(json!({"type":"response.function_call_arguments.done","output_index":i,"item_id":item["id"],"arguments":item["arguments"]}));
                }
                events
                    .push(json!({"type":"response.output_item.done","output_index":i,"item":item}));
            }
            events.push(json!({"type":"response.completed","response":reply}));
            for (i, event) in events.iter_mut().enumerate() {
                event["sequence_number"] = i.into();
            }
        }
    }
    let mut result: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    if *protocol == ProviderKind::OpenAiCompatible {
        result.push_str("data: [DONE]\n\n");
    }
    result
}
fn reply(protocol: &ProviderKind, first: bool, signature: bool) -> Value {
    let call = json!({"id":"call-one","type":"function","function":{"name":"inspect","arguments":"{\"target\":\"测试\"}"}});
    match protocol {
        ProviderKind::OpenAiCompatible => {
            let mut message = if first {
                json!({"role":"assistant","content":"检查一下","tool_calls":[call]})
            } else {
                json!({"role":"assistant","content":"检查完成"})
            };
            if signature {
                if first {
                    message["tool_calls"][0]["extra_content"] =
                        json!({"google":{"thought_signature":"original-opaque-signature"}});
                }
            } else {
                message["reasoning_content"] = "private-reasoning-原样".into();
            }
            json!({"choices":[{"index":0,"message":message,"finish_reason":if first {"tool_calls"} else {"stop"}}]})
        }
        ProviderKind::AnthropicMessages => {
            let mut content = vec![
                json!({"type":"thinking","thinking":"private-reasoning-原样","signature":"original-opaque-signature"}),
                json!({"type":"redacted_thinking","data":"opaque-redacted"}),
            ];
            content.push(if first {json!({"type":"tool_use","id":"call-one","name":"inspect","input":{"target":"测试"}})} else {json!({"type":"text","text":"检查完成"})});
            json!({"type":"message","role":"assistant","content":content,"stop_reason":if first {"tool_use"} else {"end_turn"}})
        }
        ProviderKind::OpenAiResponses => {
            let mut output = vec![
                json!({"type":"reasoning","id":"reasoning-one","summary":[],"encrypted_content":"opaque-encrypted","status":"completed"}),
            ];
            output.push(if first {json!({"type":"function_call","id":"item-one","call_id":"call-one","name":"inspect","arguments":"{\"target\":\"测试\"}","status":"completed"})} else {json!({"type":"message","id":"message-one","role":"assistant","status":"completed","content":[{"type":"output_text","text":"检查完成","annotations":[]}]})});
            json!({"object":"response","id":"response-one","status":"completed","output":output})
        }
    }
}
struct Host(AtomicUsize);
impl ToolExecutor for Host {
    fn authorize<'a>(
        &'a self,
        _: &'a ValidatedCall,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }
    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            assert_eq!(call.arguments()["target"], "测试");
            self.0.fetch_add(1, Ordering::SeqCst);
            ToolOutcome::Success(json!({"value":42}))
        })
    }
}
struct StreamContext;
impl LoopContext for StreamContext {
    fn streams_replies(&self) -> bool {
        true
    }
    fn before_model<'a>(
        &'a self,
        r: &'a ChatRequest,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, shellpilot_server::agent::tool_loop::Result<ChatRequest>> {
        Box::pin(async move { Ok(r.clone()) })
    }
    fn begin_batch<'a>(
        &'a self,
        _: &'a [ToolCall],
        _: Option<&'a str>,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, shellpilot_server::agent::tool_loop::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn started(&self, _: &str) -> shellpilot_server::agent::tool_loop::Result<()> {
        Ok(())
    }
    fn result<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, shellpilot_server::agent::tool_loop::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
async fn contract(protocol: ProviderKind, stream: bool, signature: bool) {
    let first = reply(&protocol, true, signature);
    let w = Wire {
        replies: Arc::new(Mutex::new(
            vec![first.clone(), reply(&protocol, false, signature)].into(),
        )),
        requests: Arc::default(),
        protocol: protocol.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let path = match protocol {
        ProviderKind::AnthropicMessages => "messages",
        ProviderKind::OpenAiResponses => "responses",
        _ => "chat/completions",
    };
    let config: ModelConfig = serde_json::from_value(json!({"provider":protocol,"baseUrl":format!("http://{}/v1/",listener.local_addr().unwrap()),"completionsPath":path,"model":"arbitrary-model-name","outputTokenParameter":"maxTokens","capabilities":{"developerRole":false,"temperature":false,"reasoningEffort":false},"requireApiKey":false})).unwrap();
    let store = Arc::new(Store::in_memory().unwrap());
    let models = ModelRepository::new(store.clone());
    models
        .save(
            "model",
            "test",
            0,
            ModelConfigUpdate {
                config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    let client = ModelClient::new(models.runtime("model", "test").unwrap()).unwrap();
    let app = Router::new()
        .route("/{*path}", post(respond))
        .with_state(w.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut registry = ToolRegistry::default();
    registry.register(FunctionDefinition{name:"inspect".into(),description:"test only".into(),parameters:json!({"type":"object","properties":{"target":{"type":"string"}},"required":["target"],"additionalProperties":false})}).unwrap();
    let session = Repository::new(store.clone())
        .create_general_session_with_policy("model", "test", Some(Policy::FullAccess))
        .unwrap();
    let mut run = ToolLoop::new(
        &registry,
        vec![ChatMessage::User {
            content: "检查测试项".into(),
        }],
        Limits::default(),
    )
    .unwrap()
    .with_approval_boundary(ApprovalBoundary::new(
        Approvals::new(store),
        session.id,
        "test".into(),
    ));
    let context = StreamContext;
    if stream {
        run = run.with_context(&context);
    }
    let host = Host(0.into());
    let logs = tempfile::tempdir().unwrap();
    let diagnostics = Arc::new(shellpilot_server::diagnostics::Diagnostics::default());
    diagnostics
        .configure(
            logs.path().into(),
            shellpilot_server::diagnostics::Level::Debug,
        )
        .unwrap();
    let trace = diagnostics.trace("continuation-test", "chat", "model");
    let result = trace
        .scope(run.run(&client, &host, &CancellationToken::new()))
        .await
        .unwrap_or_else(|e| {
            panic!(
                "{protocol:?} stream={stream} signatures={signature} requests={} error={e:?}",
                w.requests.lock().unwrap().len()
            )
        });
    assert_eq!(result.content.as_deref(), Some("检查完成"));
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    let visible = serde_json::to_string(&result).unwrap();
    assert!(!visible.contains("private-reasoning") && !visible.contains("opaque"));
    let log = std::fs::read_to_string(
        logs.path()
            .join(chrono::Local::now().format("%Y-%m-%d").to_string())
            .join("continuation-test.log"),
    )
    .unwrap();
    assert!(
        !log.contains("private-reasoning") && !log.contains("opaque"),
        "reasoning and signatures are protocol state, never diagnostic text"
    );
    let requests = w.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let next = &requests[1];
    match protocol {
        ProviderKind::OpenAiCompatible => {
            let replay = next["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "assistant")
                .unwrap();
            assert_eq!(replay, &first["choices"][0]["message"]);
        }
        ProviderKind::AnthropicMessages => {
            assert_eq!(next["messages"][1]["content"], first["content"])
        }
        ProviderKind::OpenAiResponses => {
            let items = next["input"].as_array().unwrap();
            assert_eq!(&items[1..3], first["output"].as_array().unwrap());
            assert_eq!(items[3]["type"], "function_call_output");
            assert_eq!(items[3]["call_id"], "call-one");
            assert_eq!(next["tools"][0]["type"], "function");
        }
    }
    task.abort();
}
#[tokio::test]
async fn chat_reasoning_roundtrip_json_and_stream() {
    for stream in [false, true] {
        contract(ProviderKind::OpenAiCompatible, stream, false).await;
    }
}
#[tokio::test]
async fn gemini_signature_roundtrip_json_and_stream() {
    for stream in [false, true] {
        contract(ProviderKind::OpenAiCompatible, stream, true).await;
    }
}
#[tokio::test]
async fn anthropic_signed_thinking_roundtrip_json_and_stream() {
    for stream in [false, true] {
        contract(ProviderKind::AnthropicMessages, stream, false).await;
    }
}
#[tokio::test]
async fn responses_reasoning_function_roundtrip_json_and_stream() {
    for stream in [false, true] {
        contract(ProviderKind::OpenAiResponses, stream, false).await;
    }
}

async fn endpoint(protocol: ProviderKind, store: Arc<Store>, profile: &str) -> (Wire,tokio::task::JoinHandle<()>) {
    let wire=Wire{replies:Arc::new(Mutex::new(vec![reply(&protocol,false,false)].into())),requests:Arc::default(),protocol:protocol.clone()};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let path=match protocol{ProviderKind::OpenAiCompatible=>"chat/completions",ProviderKind::AnthropicMessages=>"messages",ProviderKind::OpenAiResponses=>"responses"};
    let config:ModelConfig=serde_json::from_value(json!({"provider":protocol,"baseUrl":format!("http://{}/v1/",listener.local_addr().unwrap()),"completionsPath":path,"model":profile,"outputTokenParameter":"maxTokens","capabilities":{"developerRole":false,"temperature":false,"reasoningEffort":false},"requireApiKey":false})).unwrap();
    ModelRepository::new(store).save(profile,"test",0,ModelConfigUpdate{config,api_key:KeyChange::Keep}).unwrap();
    let app=Router::new().route("/{*path}",post(respond)).with_state(wire.clone());
    (wire,tokio::spawn(async move{axum::serve(listener,app).await.unwrap()}))
}
#[tokio::test]
async fn same_conversation_continues_after_all_cross_protocol_model_changes() {
    use shellpilot_server::agent::chat::{Manager,Request};
    let protocols=[ProviderKind::OpenAiCompatible,ProviderKind::AnthropicMessages,ProviderKind::OpenAiResponses];
    for from in &protocols {for to in &protocols {if from==to{continue;}
        for tools in [false,true] {
            let dir=tempfile::tempdir().unwrap();let path=dir.path().join("switch.sqlite");
            let store=Arc::new(Store::open(&path).unwrap());
            let (old,old_server)=endpoint(from.clone(),store.clone(),"old").await;
            let (next,next_server)=endpoint(to.clone(),store.clone(),"next").await;
            let repo=Repository::new(store.clone());
            let session=repo.create_general_session_with_policy("old","test",Some(Policy::Deny)).unwrap();
            let manager=Manager::new(store.clone(),CancellationToken::new());
            let mut request=Request{agent_id:"old".into(),user_id:"test".into(),session_id:session.id.clone(),request_id:"turn-one".into(),terminal_session_id:String::new(),message:"记住端口是 12233".into(),attachments:vec![]};
            manager.chat_desktop(request.clone(),None,tools,&CancellationToken::new(),|_|async{Ok(())}).await.unwrap();
            let history=manager.history("old".into(),"test".into(),session.id.clone(),0,20).await.unwrap();
            let before=repo.events(&session.id,"test",0,200).unwrap();
            let switched=manager.change_model("test".into(),session.id.clone(),"old".into(),"next".into(),history.session.revision).await.unwrap();
            assert_eq!(switched.id,session.id);assert_eq!(switched.scope,history.session.scope);assert_eq!(switched.task,history.session.task);
            assert!(manager.history("old".into(),"test".into(),session.id.clone(),0,20).await.is_err());
            drop(manager);drop(repo);drop(store);
            let store=Arc::new(Store::open(&path).unwrap());
            let manager=Manager::new(store.clone(),CancellationToken::new());
            request.agent_id="next".into();request.request_id="turn-two".into();request.message="刚才提到的端口是多少？".into();
            manager.chat_desktop(request,None,tools,&CancellationToken::new(),|_|async{Ok(())}).await.unwrap_or_else(|e|panic!("{from:?} -> {to:?} tools={tools}: {e:?}"));
            let history=manager.history("next".into(),"test".into(),session.id.clone(),0,20).await.unwrap();
            assert_eq!(history.turns.len(),2);assert_eq!(history.turns[0].message,"记住端口是 12233");assert_eq!(history.turns[0].result_text.as_deref(),Some("检查完成"));
            assert_eq!(&Repository::new(store).events(&session.id,"test",0,200).unwrap()[..before.len()],before.as_slice());
            let requests=next.requests.lock().unwrap();assert_eq!(requests.len(),1);
            let sent=requests[0].to_string();assert!(sent.contains("12233")&&sent.contains("检查完成"));
            assert!(!sent.contains("private-reasoning")&&!sent.contains("original-opaque-signature")&&!sent.contains("opaque-encrypted"));
            assert_eq!(old.requests.lock().unwrap().len(),1);
            old_server.abort();next_server.abort();
        }
    }}
}
