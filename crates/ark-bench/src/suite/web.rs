//! The `web` workload: find a page with `web.search`, read it with `web.fetch`, find the
//! passage with `get_evidence`, answer with the source. The pages come from a server on
//! loopback and the search from a fixed engine, so a run never touches the network.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, OnceLock};

use ark_sdk::prelude::ark_tools;
use ark_sdk::prelude::*;
use ark_tools::web::{Hit, SearchBackend, WebAdapter};
use serde_json::json;

use super::{answer, has, one, Spec};
use crate::sim::{cite_all, competent, Goal, Policy, SimRequest};
use crate::workload::{Env, Mode, RunOutput, TurnSpec, Verdict, Workload};

/// The page the answer is on: a long guide with the fact far below what its card shows.
fn guide() -> String {
    let mut lines = vec![
        "<html><head><title>A tea brewing guide</title></head><body><main><h1>Brewing tea</h1>"
            .to_string(),
    ];
    for i in 1..=90 {
        lines.push(format!(
            "<p>Note {i}: water quality, leaf size and the shape of the pot all change how a cup tastes, so keep notes as you go.</p>"
        ));
    }
    lines.push("<h2>Temperatures</h2><p>Green tea: steep at 80 °C (175 °F) for two minutes.</p><p>Black tea: steep at 95 °C for four minutes.</p>".into());
    lines.push("</main></body></html>".into());
    lines.join("\n")
}

/// The loopback server's address, started once per process.
fn server() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let page = guide();
        std::thread::spawn(move || {
            for mut sock in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
                    page.len()
                );
                let _ = sock.write_all(reply.as_bytes());
            }
        });
        base
    })
}

/// A search engine with fixed results.
struct FixedSearch;

#[async_trait::async_trait]
impl SearchBackend for FixedSearch {
    fn name(&self) -> String {
        "fixed".into()
    }
    async fn search(&self, _query: &str, limit: usize) -> ark_tools::Result<Vec<Hit>> {
        let base = server();
        let hits = [
            (
                "A tea brewing guide",
                "guide",
                "Temperatures and times for every kind of tea.",
            ),
            ("Tea shop: green teas", "shop", "Buy green tea online."),
            ("The history of tea", "history", "How tea reached Europe."),
        ];
        Ok(hits
            .iter()
            .take(limit)
            .map(|(title, path, snippet)| Hit {
                title: title.to_string(),
                url: format!("{base}/{path}"),
                snippet: snippet.to_string(),
            })
            .collect())
    }
}

/// The first loopback link in the latest search result.
fn first_link(r: &SimRequest) -> Option<String> {
    let text = r.result_of("web.search")?;
    let start = text.find("http://127.0.0.1")?;
    let link: String = text[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    Some(link)
}

pub struct WebSpec(Spec);

pub fn web() -> WebSpec {
    WebSpec(Spec {
        name: "web",
        class: "web",
        description:
            "find a page on the web, read it, and answer from the passage that holds the fact",
        turns: one("At what water temperature should green tea be brewed? Say which page says so."),
        policy: competent(
            |r| {
                let mut goals = vec![Goal::call(
                    "web.search",
                    json!({ "query": "green tea brewing temperature" }),
                )];
                if let Some(url) = first_link(r) {
                    goals.push(Goal::call("web.fetch", json!({ "url": url })));
                    goals.push(Goal::find("80 °C", "web.fetch", "green tea"));
                }
                goals
            },
            answer(|r| {
                if r.results()
                    .iter()
                    .any(|(_, t)| t.contains("Green tea: steep at 80 °C"))
                {
                    format!(
                        "Brew green tea at 80 °C, says A tea brewing guide. {}",
                        cite_all(r)
                    )
                } else {
                    "I could not find the temperature on the web.".into()
                }
            }),
        ),
        prepare: None,
        check: Arc::new(|o: &RunOutput| {
            Verdict::all(&[
                (
                    has(o.answer(), "80 °C"),
                    "wrong or missing temperature (expected 80 °C)",
                ),
                (o.executed("web.fetch") >= 1, "never read a page"),
            ])
        }),
    })
}

impl Workload for WebSpec {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn class(&self) -> &str {
        self.0.class()
    }
    fn description(&self) -> &str {
        self.0.description()
    }
    fn turns(&self, mode: Mode) -> Vec<TurnSpec> {
        self.0.turns(mode)
    }
    fn policy(&self) -> Policy {
        self.0.policy()
    }
    fn build(&self, env: &Env) -> anyhow::Result<Ark> {
        let web = WebAdapter::new()
            .backends(vec![Arc::new(FixedSearch)])
            .allow_private(true);
        Ok(super::builder(env).web_with(web).build()?)
    }
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        self.0.prepare(ark)
    }
    fn check(&self, out: &RunOutput) -> Verdict {
        self.0.check(out)
    }
}
