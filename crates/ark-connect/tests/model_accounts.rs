#![cfg(unix)]

use ark_connect::{ApiKeys, MemorySecrets};
use ark_model::{accounts, Cli, CliModel, Message, Provider, SessionBackend};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

#[tokio::test]
async fn account_scopes_survive_restart_and_revoke_calls_processes_and_old_attempts() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    let keys = ApiKeys::open(root.path(), secrets.clone());
    ark_model::keys::set_source(Arc::new(keys.clone()));
    let request_a = uuid::Uuid::new_v4().to_string();
    let request_b = uuid::Uuid::new_v4().to_string();
    let a = keys
        .add_account("claude", &request_a, "Personal", "")
        .await
        .unwrap();
    let b = keys
        .add_account("claude", &request_b, "Work", "")
        .await
        .unwrap();
    let setup = accounts::Lease::login(&a).unwrap();
    assert!(accounts::Lease::login(&a).is_err());
    drop(setup);
    let a = keys
        .accounts
        .activate(&a, Some("personal@example.test"))
        .unwrap();
    let b = keys
        .accounts
        .activate(&b, Some("work@example.test"))
        .unwrap();
    assert!(
        accounts::Lease::login(&a).is_err(),
        "active account cannot be signed into without invalidation"
    );
    assert_ne!(a.id, b.id);
    assert_eq!(
        keys.add_account("claude", &request_a, "Ignored retry label", "")
            .await
            .unwrap(),
        a
    );
    assert_eq!(keys.accounts.list().unwrap().len(), 2);
    assert_eq!(
        ApiKeys::open(root.path(), secrets.clone())
            .accounts
            .list()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        keys.accounts
            .directory(&a.id)
            .unwrap()
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(keys
        .add_account("claude", "../escape", "", "")
        .await
        .is_err());
    assert!(keys
        .add_account("unknown", &uuid::Uuid::new_v4().to_string(), "", "")
        .await
        .is_err());
    assert!(keys.accounts.rename(&a.id, "\n").is_err());
    let mut command = ark_os::process::command(std::path::Path::new("/bin/sh"));
    a.command(&mut command).unwrap();
    let env: Vec<_> = command.as_std().get_envs().collect();
    assert!(env
        .iter()
        .any(|(name, value)| *name == "ANTHROPIC_API_KEY" && value.is_none()));
    assert!(env.iter().any(|(name, value)| *name == "CLAUDE_CONFIG_DIR"
        && value.map(std::path::Path::new)
            == Some(keys.accounts.directory(&a.id).unwrap().as_path())));

    let binary = root.path().join("claude-fixture");
    std::fs::write(&binary, format!(r##"#!/bin/sh
set -eu
[ -z "${{ANTHROPIC_API_KEY-}}" ]
[ -z "${{CLAUDE_CODE_OAUTH_TOKEN-}}" ]
case "$CLAUDE_CONFIG_DIR" in *{id}) utilization=0.7;; *) utilization=0.1;; esac
echo $$ > "$CLAUDE_CONFIG_DIR/pid"
session=fixture
while [ "$#" -gt 0 ]; do
  if [ "$1" = --session-id ]; then shift; session="$1"; fi
  shift
done
while IFS= read -r line; do
  echo '{{"type":"rate_limit_event","rate_limit_info":{{"status":"allowed","unifiedWindows":{{"five_hour":{{"utilization":'"$utilization"',"resetsAt":1999999999}}}}}}}}'
  echo '{{"type":"result","subtype":"success","session_id":"'"$session"'","structured_output":{{"action":"reply","text":"scoped","tool_calls":[]}},"num_turns":1,"usage":{{"input_tokens":10,"output_tokens":3}}}}'
done
"##, id=b.id)).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let model_a = Arc::new(
        CliModel::for_account(Cli::Claude, "sonnet", a.clone())
            .unwrap()
            .with_program(&binary),
    );
    let model_b = Arc::new(
        CliModel::for_account(Cli::Claude, "sonnet", b.clone())
            .unwrap()
            .with_program(&binary),
    );
    let messages_a = [Message::user("a")];
    let messages_b = [Message::user("b")];
    let (first, second) = tokio::join!(
        model_a.open("", &[], &messages_a),
        model_b.open("", &[], &messages_b)
    );
    assert_eq!(first.unwrap().1.content, "scoped");
    assert_eq!(second.unwrap().1.content, "scoped");
    assert_eq!(
        ark_model::usage::usage_of(&a.id).unwrap().windows[0].used_percent,
        10
    );
    assert_eq!(
        ark_model::usage::usage_of(&b.id).unwrap().windows[0].used_percent,
        70
    );
    assert!(model_a.model_id().starts_with(&a.id));
    assert!(model_b.model_id().starts_with(&b.id));
    assert_eq!(accounts::active(&b.id), 0);
    let pid_b: u32 = std::fs::read_to_string(keys.accounts.directory(&b.id).unwrap().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(ark_os::process::alive(pid_b));
    let held = accounts::Lease::acquire(&b).unwrap();
    keys.accounts.begin_reconnect(&b.id).unwrap();
    assert!(
        ark_model::usage::usage_of(&b.id).is_none(),
        "invalidated telemetry is filtered without relying on forget ordering"
    );
    assert!(
        accounts::Lease::login(&keys.accounts.find(&b.id).unwrap()).is_err(),
        "login waits for the previous execution lease"
    );
    assert!(accounts::Lease::acquire(&b).is_err());
    assert!(keys.accounts.reconnect(&b.id).is_err());
    assert!(accounts::stop_processes(&b.id).await.is_err());
    drop(held);
    accounts::stop_processes(&b.id).await.unwrap();
    assert!(
        !ark_os::process::alive(pid_b),
        "idle vendor process stopped before credentials change"
    );
    let renewed = keys.accounts.find(&b.id).unwrap();
    assert!(keys.accounts.activate(&renewed, None).is_err());
    assert!(keys
        .accounts
        .activate(&renewed, Some("different@example.test"))
        .is_err());
    let renewed = keys
        .accounts
        .activate(&renewed, Some("work@example.test"))
        .unwrap();
    assert!(renewed.revision > b.revision);
    assert!(
        ark_model::usage::usage_of(&b.id).is_none(),
        "old telemetry remains hidden after reactivation"
    );
    assert!(b.check().is_err());
    assert!(model_b
        .complete("", &[Message::user("stale")], &[])
        .await
        .is_err());
    assert!(a.check().is_ok());
    keys.accounts.begin_remove(&b.id).unwrap();
    assert!(keys.accounts.begin_reconnect(&b.id).is_err());
    keys.accounts.finish_remove(&b.id).unwrap();
    keys.accounts.finish_remove(&b.id).unwrap();
    assert!(keys
        .add_account("claude", &request_b, "Resurrect", "")
        .await
        .is_err());
    assert!(
        ark_model::is_provider(&b.id),
        "removed account syntax remains explicit"
    );
    assert!(ark_model::build(&b.id, Some("sonnet".into())).is_err());
    let records: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.path().join("model-accounts/accounts.json")).unwrap(),
    )
    .unwrap();
    let tombstone = records
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == b.id)
        .unwrap();
    assert!(tombstone["identity"].is_null());
    assert_eq!(tombstone["name"], "");
    assert_eq!(keys.accounts.list().unwrap().len(), 1);
    assert!(a.check().is_ok());
    accounts::stop_processes(&a.id).await.unwrap();

    let api = keys
        .add_account(
            "opencode-go",
            &uuid::Uuid::new_v4().to_string(),
            "API one",
            "first-test-key",
        )
        .await
        .unwrap();
    let other = keys
        .add_account(
            "opencode-go",
            &uuid::Uuid::new_v4().to_string(),
            "API two",
            "second-test-key",
        )
        .await
        .unwrap();
    assert_eq!(api.key().unwrap(), "first-test-key");
    assert_eq!(other.key().unwrap(), "second-test-key");
    let serialized =
        std::fs::read_to_string(root.path().join("model-accounts/accounts.json")).unwrap();
    assert!(!serialized.contains("first-test-key"));
    assert!(!serialized.contains("second-test-key"));
    keys.accounts.begin_reconnect(&api.id).unwrap();
    let replaced = keys
        .accounts
        .replace_key(&api.id, "replacement-test-key")
        .unwrap();
    assert_eq!(replaced.key().unwrap(), "replacement-test-key");
    assert_eq!(other.key().unwrap(), "second-test-key");
    assert!(api.key().is_err());
    keys.accounts.begin_remove(&replaced.id).unwrap();
    keys.accounts.finish_remove(&replaced.id).unwrap();
    assert!(replaced.key().is_err());
    assert_eq!(other.key().unwrap(), "second-test-key");
    let codex_a = keys
        .add_account("codex", &uuid::Uuid::new_v4().to_string(), "Codex one", "")
        .await
        .unwrap();
    let codex_a = keys
        .accounts
        .activate(&codex_a, Some("codex-one@example.test"))
        .unwrap();
    let codex_b = keys
        .add_account("codex", &uuid::Uuid::new_v4().to_string(), "Codex two", "")
        .await
        .unwrap();
    let codex_b = keys
        .accounts
        .activate(&codex_b, Some("codex-two@example.test"))
        .unwrap();
    let codex_binary = root.path().join("codex-fixture");
    std::fs::write(&codex_binary, format!(r##"#!/usr/bin/env python3
import os,sys,json,pathlib
scope=pathlib.Path(os.environ['CODEX_HOME'])
assert not os.environ.get('OPENAI_API_KEY')
assert not os.environ.get('CODEX_API_KEY')
percent=80 if scope.name=='{second}' else 20
if sys.argv[1]=='app-server':
 for line in sys.stdin:
  request=json.loads(line)
  if 'id' not in request: continue
  method=request.get('method')
  if method=='initialize': result={{}}
  elif method=='account/rateLimits/read': result={{'rateLimits':{{'primary':{{'usedPercent':percent,'windowDurationMins':300,'resetsAt':1999999999}},'planType':'plus'}}}}
  else:
   print(json.dumps({{'id':request['id'],'error':{{'code':-32601,'message':'Fixture forces the isolated exec fallback'}}}}),flush=True);continue
  print(json.dumps({{'id':request['id'],'result':result}}),flush=True)
else:
 sys.stdin.read()
 print(json.dumps({{'type':'thread.started','thread_id':'11111111-1111-4111-8111-111111111111'}}))
 print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':json.dumps({{'action':'reply','text':scope.name,'tool_calls':[]}})}}}}))
 print(json.dumps({{'type':'turn.completed','usage':{{'input_tokens':10,'output_tokens':2}}}}))
"##, second=codex_b.id)).unwrap();
    std::fs::set_permissions(&codex_binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let first_codex = CliModel::for_account(Cli::Codex, "default", codex_a.clone())
        .unwrap()
        .with_program(&codex_binary);
    let second_codex = CliModel::for_account(Cli::Codex, "default", codex_b.clone())
        .unwrap()
        .with_program(&codex_binary);
    let first_reply = first_codex
        .open("", &[], &[Message::user("one")])
        .await
        .unwrap();
    let second_reply = second_codex
        .open("", &[], &[Message::user("two")])
        .await
        .unwrap();
    assert_eq!(first_reply.1.content, codex_a.id);
    assert_eq!(second_reply.1.content, codex_b.id);
    assert_eq!(
        ark_model::usage::usage_of(&codex_a.id).unwrap().windows[0].used_percent,
        20
    );
    assert_eq!(
        ark_model::usage::usage_of(&codex_b.id).unwrap().windows[0].used_percent,
        80
    );

    let writes = std::sync::Barrier::new(4);
    let before_concurrent = keys.accounts.list().unwrap().len();
    std::thread::scope(|scope| {
        for index in 0..4 {
            let another = ApiKeys::open(root.path(), secrets.clone());
            let writes = &writes;
            scope.spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                writes.wait();
                runtime
                    .block_on(another.add_account(
                        "opencode-go",
                        &uuid::Uuid::new_v4().to_string(),
                        &format!("Concurrent {index}"),
                        "concurrent-fixture-key",
                    ))
                    .unwrap();
            });
        }
    });
    assert_eq!(keys.accounts.list().unwrap().len(), before_concurrent + 4);

    let search_binary = root.path().join("search-fixture");
    std::fs::write(
        &search_binary,
        "#!/bin/sh\necho $$ > \"$CLAUDE_CONFIG_DIR/search-pid\"\nexec sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&search_binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let search_model = CliModel::for_account(Cli::Claude, "sonnet", a.clone())
        .unwrap()
        .with_program(&search_binary);
    let search_task =
        tokio::spawn(async move { Provider::web_search(&search_model, "fixture", 1).await });
    let marker = keys.accounts.directory(&a.id).unwrap().join("search-pid");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let search_pid: u32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(accounts::active(&a.id), 1);
    keys.accounts.begin_reconnect(&a.id).unwrap();
    assert!(
        accounts::stop_processes(&a.id).await.is_err(),
        "search must drain before reconnect"
    );
    search_task.abort();
    assert!(search_task.await.unwrap_err().is_cancelled());
    assert_eq!(accounts::active(&a.id), 0);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while ark_os::process::alive(search_pid) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    std::fs::write(root.path().join("model-accounts/accounts.json"), b"broken").unwrap();
    assert!(
        keys.accounts.list().is_err(),
        "corrupt account records fail closed"
    );
}
