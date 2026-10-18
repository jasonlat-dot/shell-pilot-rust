//! Explicitly opted-in E6 checks. Source settings are opened read-only and copied
//! into an in-memory Store. Never included in ordinary regression runs.
use super::*;
use crate::{
    agent::{ssh_tools, Target, TaskRequest},
    sftp::{self, model::AccessMode},
    terminal::{self, TerminalManager},
};
use futures_util::future::BoxFuture;
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection, OpenFlags};
use std::sync::atomic::Ordering;

const READ_ONLY_COMMAND: &str = "id -un; pwd; uname -s";

fn frozen() -> Arc<Store> {
    let path = std::env::var("SHELLPILOT_E6_DATABASE")
        .expect("explicit SHELLPILOT_E6_DATABASE is required");
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let store = Arc::new(Store::in_memory().unwrap());
    {
        let target = store.database.lock().unwrap();
        // Restrict credentials to the single localhost WSL connection explicitly
        // chosen for this acceptance run; never try other saved accounts.
        for (table, filter) in [
            ("ssh_connections", "user_id='default' AND json_extract(metadata,'$.connectionName')='wsl' AND json_extract(metadata,'$.host')='127.0.0.1' AND json_extract(metadata,'$.username')='jasonlat'"),
            ("agent_extension_settings", "user_id='default'"),
            ("agent_extension_credentials", "user_id='default' AND server_name='baidu-search'"),
            ("agent_mcp_connection_values", "user_id='default' AND server_name='baidu-search'"),
        ] {
            let mut statement = source.prepare(&format!("SELECT * FROM {table} WHERE {filter}")).unwrap();
            let count = statement.column_count();
            let placeholders = vec!["?"; count].join(",");
            let rows = statement.query_map([], |row| {
                (0..count).map(|i| row.get::<_, SqlValue>(i)).collect::<rusqlite::Result<Vec<_>>>()
            }).unwrap();
            for row in rows {
                target.execute(&format!("INSERT INTO {table} VALUES({placeholders})"), params_from_iter(row.unwrap())).unwrap();
            }
        }
    }
    assert_eq!(
        store.list("default").unwrap().len(),
        1,
        "WSL selection must be unique"
    );
    store
}

struct ExactReadOnlyPermit(AtomicUsize);
impl ssh_tools::Authorizer for ExactReadOnlyPermit {
    fn authorize<'a>(
        &'a self,
        request: ssh_tools::AuthorizationRequest<'a>,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        assert_eq!(request.command.command, READ_ONLY_COMMAND);
        assert_eq!(request.binding.identity.username, "jasonlat");
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { true })
    }
}

#[tokio::test]
#[ignore = "E6 real localhost SSH; requires explicit read-only source database"]
async fn e6_live_ssh_identity_and_replay() {
    let store = frozen();
    let connection = store.list("default").unwrap().remove(0);
    let terminals = Arc::new(TerminalManager::new(store.clone()));
    let opened = terminals
        .connect(terminal::OpenRequest {
            connection_id: connection.connection_id.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
            cols: None,
            rows: None,
        })
        .await
        .unwrap();
    let repo = Repository::new(store.clone());
    let session = repo.create_session("e6", "default").unwrap();
    repo.start_task(
        &session.id,
        "default",
        session.revision,
        &uuid::Uuid::new_v4().to_string(),
        TaskRequest {
            goal: "E6 read-only SSH identity acceptance".into(),
            constraints: vec!["No remote mutations".into()],
            target: Some(Target {
                connection_id: connection.connection_id,
                terminal_session_id: opened.session_id,
            }),
        },
    )
    .unwrap();
    let session = repo.session(&session.id, "default").unwrap();
    let permit = Arc::new(ExactReadOnlyPermit(AtomicUsize::new(0)));
    let executor = ssh_tools::SshExecutor::bind(
        store,
        terminals.clone(),
        &session.id,
        "default",
        &session.task.unwrap().id,
    )
    .await
    .unwrap()
    .with_authorizer(permit.clone());
    let command = ssh_tools::Command {
        command: READ_ONLY_COMMAND.into(),
        timeout_seconds: 15,
        explanation: Some("Read identity, home and OS only".into()),
    };
    let cancel = CancellationToken::new();
    let result = executor
        .execute_command("e6-readonly-identity", &command, &cancel)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("jasonlat"));
    assert!(result.stdout.contains("Linux"));
    let replay = executor
        .execute_command("e6-readonly-identity", &command, &cancel)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    assert_eq!(permit.0.load(Ordering::SeqCst), 1);
    println!("E6 SSH: login identity and read-only exec verified; replay does not execute again");
    terminals.shutdown().await;
}

async fn sftp_read_only(mode: AccessMode) {
    let store = frozen();
    let connection = store.list("default").unwrap().remove(0);
    let sudo_password = if mode == AccessMode::SudoRoot
        && std::env::var("SHELLPILOT_E6_SUDO_USE_LOGIN_PASSWORD").as_deref() == Ok("1")
    {
        store
            .credentials(&connection.connection_id)
            .unwrap()
            .password
    } else {
        None
    };
    let manager = sftp::Manager::new(store);
    let session = manager
        .open(sftp::model::OpenRequest {
            connection_id: connection.connection_id,
            access_mode: mode,
            sudo_password,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(session.login_username, "jasonlat");
    assert_eq!(
        session.access_username,
        if mode == AccessMode::Login {
            "jasonlat"
        } else {
            "root"
        }
    );
    let entries = manager
        .entries(&session.sftp_session_id, None)
        .await
        .unwrap();
    assert!(!entries.is_empty());
    println!(
        "E6 SFTP: mode={mode:?}, directory metadata readable, entry count={}",
        entries.len()
    );
    manager.shutdown();
}

#[tokio::test]
#[ignore = "E6 real localhost SFTP; reads directory metadata only"]
async fn e6_live_sftp_login() {
    sftp_read_only(AccessMode::Login).await;
}

#[tokio::test]
#[ignore = "E6 real localhost sudo SFTP; optional explicit login-password opt-in, no remote file mutations"]
async fn e6_live_sftp_sudo_root() {
    sftp_read_only(AccessMode::SudoRoot).await;
}

#[tokio::test]
#[ignore = "E6 real configured MCP discovery and one fixed public-information search"]
async fn e6_live_mcp_discovery_and_public_search() {
    let store = frozen();
    let settings = settings(&store, "default").unwrap();
    let server = settings
        .config
        .servers
        .iter()
        .find(|s| s.enabled && s.name == "baidu-search")
        .expect("enabled baidu-search required");
    assert!(matches!(
        server.transport,
        config::Transport::StreamableHttp { .. }
    ));
    let bearer = credentials::resolve(&store, "default", server, settings.revision).unwrap();
    let values = connection_values::resolve(&store, "default", server, settings.revision).unwrap();
    let cancel = CancellationToken::new();
    let mut client = mcp::Client::connect(server, &cancel, bearer, values).await.unwrap();
    let result = client.rpc("tools/list", json!({}), &cancel).await.unwrap();
    let tools = result["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    println!(
        "E6 MCP: initialize + tools/list passed, tool count={}",
        tools.len()
    );
    // Tool metadata only: never print endpoint, headers, stored secrets or content.
    for tool in tools {
        println!(
            "E6 MCP schema: {}",
            json!({"name": tool["name"], "inputSchema": tool["inputSchema"]})
        );
    }
    let search = tools
        .iter()
        .find(|t| t["name"] == "chatCompletions")
        .expect("expected configured search tool");
    let arguments = json!({"query":"Model Context Protocol 官方文档", "resource_type_filter":[{"type":"web","top_k":1}]});
    let arguments =
        tool_loop::validate_external_arguments(&search["inputSchema"], &arguments.to_string())
            .expect("real MCP schema and arguments must be supported by the product validator");
    let result = client
        .rpc(
            "tools/call",
            json!({"name":"chatCompletions", "arguments":arguments}),
            &cancel,
        )
        .await
        .unwrap();
    assert_ne!(result["isError"], true, "MCP reported a tool error");
    assert!(!result["content"].as_array().unwrap().is_empty());
    assert!(result["content"].as_array().unwrap().iter().any(|item| {
        item["type"] == "text" && item["text"].as_str().is_some_and(|text| !text.trim().is_empty())
    }), "search must return non-empty text content");
    for item in result["content"].as_array().unwrap() {
        if let Some(text) = item["text"].as_str() {
            if let Ok(body) = serde_json::from_str::<Value>(text) {
                println!("E6 MCP public search payload keys: {:?}", body.as_object().map(|o| o.keys().collect::<Vec<_>>()));
                for key in ["error_code", "errorCode", "code"] {
                    if let Some(code) = body.get(key) {
                        assert!(code == 0 || code == "0" || code == 200 || code == "200", "MCP search returned a business error code ({key}); value withheld");
                    }
                }
                assert!(body.get("error").is_none_or(|e| e.is_null()), "MCP search returned an error envelope");
            }
        }
    }
    println!(
        "E6 MCP: schema-validated fixed public search completed, response bytes={}",
        result.to_string().len()
    );
}

#[test]
#[ignore = "E6 reads configured local Skill metadata and one SKILL.md; no script execution"]
fn e6_live_skills_progressive_read() {
    let store = frozen();
    let settings = settings(&store, "default").unwrap();
    let skills = skills::discover(&settings.config.skill_roots);
    assert!(!skills.is_empty());
    let value = skills::read(&skills, &skills[0].id, "").unwrap();
    assert!(!value["content"].as_str().unwrap().is_empty());
    println!(
        "E6 Skills: {} discovered, first SKILL.md read successfully",
        skills.len()
    );
}
