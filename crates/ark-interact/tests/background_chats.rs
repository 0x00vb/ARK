//! A conversation made for work an agent handed to the background (ADR 0118) is in no list of
//! the person's chats: not Recent chats, not the project's sessions, not `/history`.

use ark_interact::interaction::{history, list_sessions, project_sessions};
use ark_sdk::prelude::*;

#[test]
fn a_background_conversation_is_in_no_list_of_the_persons_chats() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder().store_root(dir.path()).build().unwrap();
    let project = ark.create_project("Books", "test").unwrap();
    let engine = ark.engine();
    let turn = |id: &str, background: bool| {
        let mut s = Session::start(engine, &project.id).unwrap();
        s.begin_turn(engine, id).unwrap();
        if background {
            s.set_background(engine).unwrap();
        }
        s.id().to_string()
    };
    let chat = turn("a chat of the person", false);
    let lane = turn("read the filings", true);
    for (what, ids) in [
        (
            "list_sessions",
            list_sessions(engine, None, "")
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
        ),
        (
            "project_sessions",
            project_sessions(engine, project.id.as_str(), "")
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect(),
        ),
        (
            "history",
            history(engine, &project.id, &chat, "")
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect(),
        ),
    ] {
        assert!(ids.contains(&chat), "{what} lost the person's chat");
        assert!(!ids.contains(&lane), "{what} lists the background one");
    }
    // Even when it is the conversation the lists are asked about.
    let again = history(engine, &project.id, &lane, "").unwrap();
    assert!(!again.into_iter().any(|r| r.id == lane));
}
