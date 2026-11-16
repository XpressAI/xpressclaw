//! Prompt-free preparation for user-initiated Agent sign-in.
use super::*;

pub struct PreparedAuthentication {
    pub process: AcpProcess,
    pub spec: ContainerSpec,
    pub kind: String,
    // Sign-in must not race a turn, container recovery, or Agent deletion.
    pub lease: OwnedRwLockWriteGuard<()>,
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare(
    db: &Arc<Database>,
    config: &Arc<RwLock<Arc<Config>>>,
    docker: &DockerManager,
    agent_id: &str,
    processes: &ProjectAcpProcesses,
    conversations: &ConversationAcpProcesses,
    lifecycle: &NativeRuntimeLifecycle,
    callback_port: u16,
) -> Result<PreparedAuthentication> {
    let lease = lifecycle.try_quiesce_agent(agent_id).ok_or_else(|| {
        Error::Agent("The Agent is busy. Finish or stop its current work before signing in.".into())
    })?;
    AgentRegistry::new(db.clone()).get(agent_id)?;
    let snapshot = config.read().unwrap().clone();
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.name == agent_id)
        .ok_or_else(|| Error::AgentNotFound {
            name: agent_id.into(),
        })?;
    let kind = resolve_runner_kind(agent)?;
    let bootstrap = resolved_workspace(&snapshot, agent);
    let inspection = inspect_repository_for_turn(db, agent_id, &bootstrap).await?;
    let inspection = if inspection.requires_boundary_change() {
        let boundary = apply_repository_boundary_for_turn(db, agent_id, &bootstrap).await?;
        if boundary.changed {
            conversations.retire_agent_everywhere(agent_id).await;
            processes.retire_agent(agent_id).await;
            let _ = docker.stop_preserving(agent_id).await;
        }
        boundary.inspection
    } else {
        inspection
    };
    let repository = RuntimeRepository::from_inspection(inspection, agent);
    let github = if repository.active {
        discover_github_access(db, &repository.active_root).await?
    } else {
        None
    };
    let mut spec = build_spec(
        &snapshot,
        agent,
        &kind,
        docker,
        &repository,
        github.as_ref(),
    )?;
    configure_local_collaboration_access(db, &snapshot, agent, &mut spec, docker).await?;
    crate::proxy::service::configure_runner(
        docker,
        &snapshot.system.data_dir,
        &db.installation_id()?,
        &snapshot.proxy,
        agent,
        &mut spec,
        callback_port,
    )
    .await?;
    let built_in = is_builtin_runner_image_for_kind(&spec.image, &kind);
    if !retained_runner_image_ready(docker, agent, &mut spec, built_in).await? {
        if let Some(local) = local_runner_image_alias(&spec.image).filter(|_| built_in) {
            if runner_image_ready(docker, local, built_in, agent).await {
                spec.image = local.into();
            }
        }
        if !runner_image_ready(docker, &spec.image, built_in, agent).await {
            docker.pull_image(&spec.image).await?;
            if !runner_image_ready(docker, &spec.image, built_in, agent).await {
                return Err(Error::Backend(
                    "The selected image does not support this ACP runner configuration".into(),
                ));
            }
        }
    }
    let bundled = docker
        .image_has_label(
            &spec.image,
            "io.xpressclaw.protocol",
            BUILT_IN_RUNNER_PROTOCOL,
        )
        .await;
    configure_bundled_github_mcp(&agent.runner, &kind, bundled, &mut spec.environment)?;
    let presentations = presentation_runtime_available(docker, &spec.image).await;
    configure_codex_presentations(&kind, presentations, &mut spec.environment)?;
    if !Arc::ptr_eq(&config.read().unwrap(), &snapshot) {
        return Err(Error::Agent(
            "Agent settings changed while preparing sign-in. Please try again.".into(),
        ));
    }
    if docker
        .project_configuration_needs_recovery(agent_id, &spec)
        .await?
    {
        docker
            .recover_project_configuration(agent_id, &spec)
            .await?;
        conversations.retire_agent_everywhere(agent_id).await;
        processes.retire_agent(agent_id).await;
    }
    let process = match processes.get_or_start(docker, agent_id, &spec, &kind).await {
        Ok(runtime) => runtime.process,
        Err(error) => {
            docker.rollback_project_recovery(agent_id).await?;
            return Err(error);
        }
    };
    docker.finish_project_recovery(agent_id).await?;
    Ok(PreparedAuthentication {
        process,
        spec,
        kind,
        lease,
    })
}

/// Reconnect every lane after successful terminal authentication, as ACP requires.
pub async fn finish(
    prepared: &mut PreparedAuthentication,
    docker: &DockerManager,
    agent_id: &str,
    processes: &ProjectAcpProcesses,
    conversations: &ConversationAcpProcesses,
    reconnect: bool,
) -> Result<()> {
    conversations.retire_agent_everywhere(agent_id).await;
    if !reconnect {
        return Ok(());
    }
    processes.retire_agent(agent_id).await;
    let process = processes
        .get_or_start(docker, agent_id, &prepared.spec, &prepared.kind)
        .await?
        .process;
    process.authenticated();
    Ok(())
}

/// Stop a cancelled or failed authentication request before releasing its maintenance lease.
pub async fn cancel(agent_id: &str, processes: &ProjectAcpProcesses) {
    processes.retire_agent(agent_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workers::acp::{
        authentication::AuthenticationBroker, AcpAuthenticationStatus, AuthMethod,
        CreateElicitationResponse,
    };
    use futures_util::{FutureExt, StreamExt};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    #[ignore = "requires XPRESSCLAW_AUTH_TEST_ROOT shared with Docker/Podman and the environment fixture image"]
    async fn managed_sign_in_reconnects_without_tasks_and_cleans_up_cancelled_terminals() {
        let Ok(root) = std::env::var("XPRESSCLAW_AUTH_TEST_ROOT") else {
            return;
        };
        std::fs::create_dir_all(&root).unwrap();
        let directory = tempfile::tempdir_in(root).unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let source = directory.path().join("fixture");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("agent.cjs"), r#"
const fs=require('fs'),readline=require('readline');
const token='.auth-fixture-token';
if(process.argv.includes('--login')) {
 if(process.env.LOGIN_FIXTURE!=='one two') process.exit(8);
 console.log('LOGIN_READY');
 process.stdin.once('data',input=>{if(String(input).trim()!=='authorize')process.exit(9);fs.writeFileSync(token,'fixture');process.exit(0);});
} else {
 let login;
 const send=(id,result)=>console.log(JSON.stringify({jsonrpc:'2.0',id,result}));
 readline.createInterface({input:process.stdin}).on('line',line=>{
  const m=JSON.parse(line);
  if(m.method==='initialize') {
   if(!m.params.clientCapabilities.auth?.terminal)process.exit(10);
   send(m.id,{protocolVersion:1,agentCapabilities:{sessionCapabilities:{close:{}}},authMethods:[{id:'terminal-login',type:'terminal',name:'Fixture login',args:['--login'],env:{LOGIN_FIXTURE:'one two'}},{id:'browser',name:'Browser login'}]});
  } else if(m.method==='session/new') {
   if(fs.existsSync(token))send(m.id,{sessionId:'probe',configOptions:[]});
   else console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,error:{code:-32000,message:'Authentication required'}}));
  } else if(m.method==='session/close')send(m.id,{});
  else if(m.method==='authenticate') {
   login=m.id;console.log(JSON.stringify({jsonrpc:'2.0',id:'url-prompt',method:'elicitation/create',params:{requestId:m.id,mode:'url',message:'Enter code 1234',url:'https://example.invalid/login',elicitationId:'fixture'}}));
  } else if(m.id==='url-prompt'&&m.result?.action==='accept') {fs.writeFileSync(token,'fixture');send(login,{});}
 });
}
"#).unwrap();
        let image = std::env::var("XPRESSCLAW_ENVIRONMENT_TEST_IMAGE")
            .unwrap_or_else(|_| "xpressclaw-environment-test:local".into());
        let mut configured = Config::default();
        configured.proxy.enabled = false;
        configured.system.data_dir = directory.path().join("data");
        configured.system.workspace_dir = workspace.clone();
        configured.agents = vec![AgentConfig {
            name: "sign-in-fixture".into(),
            backend: "custom".into(),
            runner: NativeRunnerConfig {
                kind: "custom".into(),
                image,
                workspace: Some(workspace.display().to_string()),
                subscription_auth: false,
                command: vec!["node".into(), "/fixture/agent.cjs".into()],
                environment: HashMap::from([("HOME".into(), "/tmp".into())]),
                ..Default::default()
            },
            volumes: vec![format!("{}:/fixture:ro", source.display())],
            ..Default::default()
        }];
        let config = Arc::new(RwLock::new(Arc::new(configured)));
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("sign-in-fixture", "custom")
            .unwrap();
        let docker = DockerManager::connect_for_installation(&db.installation_id().unwrap())
            .await
            .unwrap();
        let processes = ProjectAcpProcesses::default();
        let conversations = ConversationAcpProcesses::default();
        let lifecycle = NativeRuntimeLifecycle::default();
        let run = std::panic::AssertUnwindSafe(async {
            let mut prepared = prepare(&db,&config,&docker,"sign-in-fixture",&processes,&conversations,&lifecycle,0).await.unwrap();
            assert!(lifecycle.try_quiesce_agent("sign-in-fixture").is_none());
            let before = processes.connected_process("sign-in-fixture","custom").await.unwrap();
            let auth = prepared.process.authentication();
            let AuthMethod::Terminal(method) = &auth.methods[0] else { panic!("terminal method missing") };
            let mut command = prepared.spec.cmd.clone().unwrap();command.extend(method.args.clone());
            let env = method.env.iter().map(|(key,value)|format!("{key}={value}")).collect::<Vec<_>>();
            let mut terminal = docker.open_authentication_terminal("sign-in-fixture",&command,&env,prepared.spec.working_dir.as_deref(),100,30).await.unwrap();
            tokio::time::timeout(Duration::from_secs(10),async {
                let mut output=String::new();
                while !output.contains("LOGIN_READY") { output.push_str(&terminal.terminal.output.next().await.unwrap().unwrap().to_string()); }
            }).await.unwrap();
            terminal.terminal.input.write_all(b"authorize\n").await.unwrap();
            tokio::time::timeout(Duration::from_secs(10),async {
                loop { if let Some(code)=docker.authentication_terminal_exit("sign-in-fixture",&terminal.socket).await.unwrap(){assert_eq!(code,0);break;} tokio::time::sleep(Duration::from_millis(100)).await; }
            }).await.unwrap();
            docker.close_authentication_terminal("sign-in-fixture",&terminal.socket).await;
            finish(&mut prepared,&docker,"sign-in-fixture",&processes,&conversations,true).await.unwrap();
            let after = processes.connected_process("sign-in-fixture","custom").await.unwrap();
            assert!(!before.same_process(&after));
            assert_eq!(after.authentication().status,AcpAuthenticationStatus::Authenticated);
            drop(prepared);

            std::fs::remove_file(workspace.join(".auth-fixture-token")).unwrap();
            let mut prepared=prepare(&db,&config,&docker,"sign-in-fixture",&processes,&conversations,&lifecycle,0).await.unwrap();
            let (broker,mut prompts)=AuthenticationBroker::new();
            let process=prepared.process.clone();
            let operation=process.authenticate("browser",None,broker.clone());tokio::pin!(operation);
            let prompt=tokio::time::timeout(Duration::from_secs(10), async { tokio::select! { prompt=prompts.recv()=>prompt.unwrap(), result=&mut operation=>panic!("authentication returned before its prompt: {result:?}") } }).await.unwrap();
            assert_eq!(serde_json::to_value(&prompt.request).unwrap()["url"],"https://example.invalid/login");
            assert!(!broker.respond("wrong-request",serde_json::from_value::<CreateElicitationResponse>(json!({"action":"accept"})).unwrap()));
            assert!(broker.respond(&prompt.id,serde_json::from_value(json!({"action":"accept"})).unwrap()));
            tokio::time::timeout(Duration::from_secs(10),&mut operation).await.unwrap().unwrap();
            finish(&mut prepared,&docker,"sign-in-fixture",&processes,&conversations,false).await.unwrap();
            let authenticated = processes.connected_process("sign-in-fixture", "custom").await.unwrap();
            assert!(process.same_process(&authenticated));
            assert_eq!(authenticated.authentication().status, AcpAuthenticationStatus::Authenticated);

            let ordinary=docker.open_project_terminal("sign-in-fixture",100,30,"ordinary").await.unwrap();
            let terminal=docker.open_authentication_terminal("sign-in-fixture",&command,&env,prepared.spec.working_dir.as_deref(),100,30).await.unwrap();
            docker.close_authentication_terminal("sign-in-fixture",&terminal.socket).await;
            assert!(docker.authentication_terminal_exit("sign-in-fixture",&terminal.socket).await.is_err());
            let sessions=docker.container_files("sign-in-fixture",json!({"operation":"sessions"})).await.unwrap();
            assert!(sessions["sessions"].as_array().unwrap().iter().any(|name| name=="ordinary"));
            drop(ordinary);
            for (command, expected) in [("exit 0", 0), ("exit 19", 19), ("kill -TERM $$", -1)] {
                let terminal = docker.open_authentication_terminal("sign-in-fixture", &["sh".into(), "-c".into(), command.into()], &[], None, 100, 30).await.unwrap();
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if let Some(code) = docker.authentication_terminal_exit("sign-in-fixture", &terminal.socket).await.unwrap() {
                            if expected < 0 { assert_ne!(code, 0); } else { assert_eq!(code, expected); }
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }).await.unwrap();
                docker.close_authentication_terminal("sign-in-fixture", &terminal.socket).await;
            }

            let (broker, mut prompts) = AuthenticationBroker::new();
            let operation = process.authenticate("browser", None, broker.clone());
            tokio::pin!(operation);
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::select! {
                    prompt = prompts.recv() => assert!(prompt.is_some()),
                    result = &mut operation => panic!("authentication returned before cancellation: {result:?}"),
                }
            }).await.unwrap();
            broker.cancel();
            cancel("sign-in-fixture", &processes).await;
            assert!(tokio::time::timeout(Duration::from_secs(10), &mut operation).await.unwrap().is_err());
            assert!(processes.connected_process("sign-in-fixture", "custom").await.is_none());

            let attempts:i64=db.with_conn(|conn|conn.query_row("SELECT count(*) FROM work_attempts",[],|row|row.get(0)).map_err(Error::from)).unwrap();
            assert_eq!(attempts,0);
        }).catch_unwind().await;
        processes.retire_agent("sign-in-fixture").await;
        conversations
            .retire_agent_everywhere("sign-in-fixture")
            .await;
        let _ = docker.remove_owned_workload("sign-in-fixture").await;
        if let Err(panic) = run {
            std::panic::resume_unwind(panic);
        }
    }
}
