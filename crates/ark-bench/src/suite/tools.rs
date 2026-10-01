//! Neutral tools for the generic workloads: a calculator, a unit converter, a small customer
//! database, a log source, a document reader, a mirror whose primary is down, a slow weather
//! service, a notes store that changes state, and filler adapters so the agent has to find the
//! right tool among realistic neighbors. No tool here knows about any application domain.

use std::sync::Arc;
use std::time::Duration;

use ark_sdk::prelude::*;
use serde_json::{json, Value};

fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "required": required, "properties": props })
}

fn s(input: &Value, k: &str) -> String {
    input
        .get(k)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

type RunFn = Arc<dyn Fn(&str, &Value, &RunCtx<'_>) -> Result<Value, String> + Send + Sync>;

/// A table-driven adapter: descriptors plus one function for every action.
struct Table {
    name: &'static str,
    actions: Vec<ActionDescriptor>,
    run: RunFn,
    delay: Duration,
}

#[async_trait::async_trait]
impl Adapter for Table {
    fn name(&self) -> &str {
        self.name
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        self.actions.clone()
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        match (self.run)(action, &input, ctx) {
            Ok(v) => Ok(ActionOutput::json(v)),
            Err(e) => Err(AdapterError::bad_input(action, e)),
        }
    }
}

fn read(name: &str, summary: &str, props: Value, required: &[&str]) -> ActionDescriptor {
    ActionDescriptor::new(name, summary, RiskTier::Read).with_input_schema(schema(props, required))
}

fn mutate(name: &str, summary: &str, props: Value, required: &[&str]) -> ActionDescriptor {
    ActionDescriptor::new(name, summary, RiskTier::Mutate)
        .with_input_schema(schema(props, required))
}

pub fn calc() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "calc",
        actions: vec![read(
            "calc.eval",
            "evaluate an arithmetic expression (+ - * / and parentheses)",
            json!({ "expr": { "type": "string", "description": "for example (2+3)*4" } }),
            &["expr"],
        )],
        run: Arc::new(|_, input, _| {
            let expr = s(input, "expr");
            let value = eval(&expr).ok_or_else(|| format!("cannot evaluate `{expr}`"))?;
            Ok(json!({ "expr": expr, "value": (value * 100.0).round() / 100.0 }))
        }),
        delay: Duration::ZERO,
    })
}

pub fn units() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "units",
        actions: vec![
            read(
                "units.convert",
                "convert a quantity between units (mi/km, lb/kg, f/c, usd/eur)",
                json!({
                    "value": { "type": "number" },
                    "from": { "type": "string", "description": "unit to convert from, e.g. mi" },
                    "to": { "type": "string", "description": "unit to convert to, e.g. km" }
                }),
                &["value", "from", "to"],
            ),
            read("units.list", "list the supported units", json!({}), &[]),
        ],
        run: Arc::new(|action, input, _| {
            if action == "units.list" {
                return Ok(json!({ "units": ["mi", "km", "lb", "kg", "f", "c", "usd", "eur"] }));
            }
            let v = input.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let (from, to) = (
                s(input, "from").to_lowercase(),
                s(input, "to").to_lowercase(),
            );
            let r = match (from.as_str(), to.as_str()) {
                ("mi", "km") => v * 1.609344,
                ("km", "mi") => v / 1.609344,
                ("lb", "kg") => v * 0.45359237,
                ("kg", "lb") => v / 0.45359237,
                ("f", "c") => (v - 32.0) * 5.0 / 9.0,
                ("c", "f") => v * 9.0 / 5.0 + 32.0,
                ("usd", "eur") => v * 0.92,
                ("eur", "usd") => v / 0.92,
                _ => return Err(format!("unsupported conversion {from} -> {to}")),
            };
            Ok(json!({ "value": v, "from": from, "to": to, "result": (r * 100.0).round() / 100.0 }))
        }),
        delay: Duration::ZERO,
    })
}

pub fn crm() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "crm",
        actions: vec![
            read(
                "crm.find_customer",
                "find a customer by name; returns id, name and tier",
                json!({ "name": { "type": "string" } }),
                &["name"],
            ),
            read(
                "crm.orders",
                "list a customer's orders with amounts",
                json!({ "customer_id": { "type": "string" } }),
                &["customer_id"],
            ),
            read(
                "crm.invoice",
                "fetch one invoice by id",
                json!({ "invoice_id": { "type": "string" } }),
                &["invoice_id"],
            ),
        ],
        run: Arc::new(|action, input, _| match action {
            "crm.find_customer" => {
                let name = s(input, "name");
                if name.to_lowercase().contains("ada") {
                    Ok(json!({ "id": "C-1042", "name": "Ada Lovelace", "tier": "gold" }))
                } else {
                    Err(format!("no customer named {name}"))
                }
            }
            "crm.orders" => {
                if s(input, "customer_id") != "C-1042" {
                    return Err("unknown customer id".into());
                }
                Ok(json!({ "customer_id": "C-1042", "orders": [
                    { "id": "O-1", "amount": 120.50 },
                    { "id": "O-2", "amount": 89.99 },
                    { "id": "O-3", "amount": 310.00 },
                    { "id": "O-4", "amount": 45.25 }
                ]}))
            }
            _ => Ok(json!({ "invoice_id": s(input, "invoice_id"), "status": "paid" })),
        }),
        delay: Duration::ZERO,
    })
}

/// The one error line in the payments log, at a fixed position among 3,000 lines.
pub const LOG_ERROR_LINE: usize = 1873;

pub fn logs() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "logs",
        actions: vec![read(
            "logs.fetch",
            "fetch today's log lines for a service",
            json!({ "service": { "type": "string" } }),
            &["service"],
        )],
        run: Arc::new(|_, input, _| {
            let service = s(input, "service");
            let lines: Vec<String> = (0..3000)
                .map(|i| {
                    if i == LOG_ERROR_LINE {
                        format!(
                            "10:{:02}:{:02} ERROR charge failed code=E4721 card=visa retry=false",
                            (i / 60) % 60,
                            i % 60
                        )
                    } else {
                        format!(
                            "10:{:02}:{:02} INFO request ok path=/v1/charge latency_ms={}",
                            (i / 60) % 60,
                            i % 60,
                            20 + i % 37
                        )
                    }
                })
                .collect();
            Ok(json!({ "service": service, "count": lines.len(), "lines": lines }))
        }),
        delay: Duration::ZERO,
    })
}

pub fn docs() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "docs",
        actions: vec![read(
            "docs.open",
            "open a stored document by artifact id and return its text",
            json!({ "artifact": { "type": "string", "description": "artifact id, e.g. art:1a2b3c" } }),
            &["artifact"],
        )],
        run: Arc::new(|_, input, ctx| {
            let id = ArtifactId(s(input, "artifact"));
            let bytes = ctx.blobs.read(&id).map_err(|e| e.to_string())?;
            Ok(json!({ "artifact": id.as_str(), "text": String::from_utf8_lossy(&bytes) }))
        }),
        delay: Duration::ZERO,
    })
}

pub fn mirror() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "mirror",
        actions: vec![
            read(
                "mirror.primary",
                "fetch a file from the primary mirror",
                json!({ "path": { "type": "string" } }),
                &["path"],
            ),
            read(
                "mirror.backup",
                "fetch a file from the backup mirror (slower, always current)",
                json!({ "path": { "type": "string" } }),
                &["path"],
            ),
        ],
        run: Arc::new(|action, input, _| match action {
            "mirror.primary" => Err("connection refused (primary mirror is down)".into()),
            _ => Ok(
                json!({ "path": s(input, "path"), "version": "4.2.1", "notes": "fixes and speedups" }),
            ),
        }),
        delay: Duration::ZERO,
    })
}

pub fn weather() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "weather",
        actions: vec![read(
            "weather.current",
            "current temperature for a city (slow service)",
            json!({ "city": { "type": "string" } }),
            &["city"],
        )],
        run: Arc::new(|_, input, _| {
            let city = s(input, "city");
            let t = match city.to_lowercase().as_str() {
                "paris" => 14,
                "tokyo" => 22,
                "lima" => 18,
                _ => return Err(format!("no station for {city}")),
            };
            Ok(json!({ "city": city, "temp_c": t }))
        }),
        delay: Duration::from_millis(250),
    })
}

pub fn notes() -> Arc<dyn Adapter> {
    Arc::new(Table {
        name: "notes",
        actions: vec![
            mutate(
                "notes.write",
                "save a note",
                json!({ "text": { "type": "string" } }),
                &["text"],
            ),
            read("notes.list", "list saved notes", json!({}), &[]),
        ],
        run: Arc::new(|action, input, _| match action {
            "notes.write" => Ok(json!({ "saved": true, "chars": s(input, "text").len() })),
            _ => Ok(json!({ "count": 0 })),
        }),
        delay: Duration::ZERO,
    })
}

/// A settings store: `settings.get` reads what `settings.set` changes.
pub fn settings() -> Arc<dyn Adapter> {
    let value = Arc::new(std::sync::Mutex::new("slow".to_string()));
    Arc::new(Table {
        name: "settings",
        actions: vec![
            read(
                "settings.get",
                "read a setting",
                json!({ "key": { "type": "string" } }),
                &["key"],
            ),
            mutate(
                "settings.set",
                "change a setting",
                json!({ "key": { "type": "string" }, "value": { "type": "string" } }),
                &["key", "value"],
            ),
        ],
        run: Arc::new(move |action, input, _| {
            let mut v = value.lock().unwrap();
            if action == "settings.set" {
                *v = s(input, "value");
                return Ok(json!({ "key": s(input, "key"), "updated": true }));
            }
            Ok(json!({ "key": s(input, "key"), "value": v.clone() }))
        }),
        delay: Duration::ZERO,
    })
}

/// Adapters the workloads never need, so discovery happens among realistic neighbors.
pub fn filler() -> Vec<Arc<dyn Adapter>> {
    let ok: RunFn = Arc::new(|action, _, _| Ok(json!({ "action": action, "ok": true })));
    let mk = |name: &'static str, actions: Vec<ActionDescriptor>| -> Arc<dyn Adapter> {
        Arc::new(Table {
            name,
            actions,
            run: ok.clone(),
            delay: Duration::ZERO,
        })
    };
    let str_prop = |d: &str| json!({ "type": "string", "description": d });
    vec![
        mk(
            "mail",
            vec![
                read(
                    "mail.search",
                    "search the mailbox",
                    json!({ "query": str_prop("search terms"), "limit": { "type": "integer" } }),
                    &["query"],
                ),
                read(
                    "mail.read",
                    "read one message",
                    json!({ "id": str_prop("message id") }),
                    &["id"],
                ),
                mutate(
                    "mail.send",
                    "send an email",
                    json!({ "to": str_prop("recipient"), "subject": str_prop("subject"), "body": str_prop("body") }),
                    &["to", "subject", "body"],
                ),
            ],
        ),
        mk(
            "calendar",
            vec![
                read(
                    "calendar.list",
                    "list events in a date range",
                    json!({ "from": str_prop("ISO date"), "to": str_prop("ISO date") }),
                    &["from", "to"],
                ),
                mutate(
                    "calendar.create",
                    "create an event",
                    json!({ "title": str_prop("title"), "start": str_prop("ISO datetime"), "minutes": { "type": "integer" } }),
                    &["title", "start"],
                ),
            ],
        ),
        mk(
            "files",
            vec![
                read(
                    "files.list",
                    "list files in a folder",
                    json!({ "path": str_prop("folder") }),
                    &["path"],
                ),
                read(
                    "files.read",
                    "read a text file",
                    json!({ "path": str_prop("file path") }),
                    &["path"],
                ),
                read(
                    "files.search",
                    "search file contents",
                    json!({ "query": str_prop("text to find"), "path": str_prop("folder") }),
                    &["query"],
                ),
            ],
        ),
        mk(
            "geo",
            vec![
                read(
                    "geo.lookup",
                    "coordinates of a place",
                    json!({ "place": str_prop("place name") }),
                    &["place"],
                ),
                read(
                    "geo.distance",
                    "distance between two places in km",
                    json!({ "from": str_prop("place"), "to": str_prop("place") }),
                    &["from", "to"],
                ),
            ],
        ),
        mk(
            "translate",
            vec![read(
                "translate.text",
                "translate text between languages",
                json!({ "text": str_prop("text"), "to": str_prop("target language code") }),
                &["text", "to"],
            )],
        ),
        mk(
            "market",
            vec![
                read(
                    "market.quote",
                    "latest price of a ticker",
                    json!({ "symbol": str_prop("ticker") }),
                    &["symbol"],
                ),
                read(
                    "market.history",
                    "daily prices of a ticker",
                    json!({ "symbol": str_prop("ticker"), "days": { "type": "integer" } }),
                    &["symbol"],
                ),
            ],
        ),
        mk(
            "tickets",
            vec![
                read(
                    "tickets.list",
                    "list open support tickets",
                    json!({ "status": str_prop("open|closed") }),
                    &[],
                ),
                mutate(
                    "tickets.create",
                    "open a support ticket",
                    json!({ "title": str_prop("title"), "body": str_prop("details") }),
                    &["title"],
                ),
            ],
        ),
    ]
}

/// Every bench adapter.
pub fn all() -> Vec<Arc<dyn Adapter>> {
    let mut v = vec![
        calc(),
        units(),
        crm(),
        logs(),
        docs(),
        mirror(),
        weather(),
        notes(),
        settings(),
    ];
    v.extend(filler());
    v
}

/// A small arithmetic evaluator: numbers, + - * /, parentheses.
pub fn eval(expr: &str) -> Option<f64> {
    struct P<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> Option<f64> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.b.get(self.i) {
                    Some(b'+') => {
                        self.i += 1;
                        v += self.term()?;
                    }
                    Some(b'-') => {
                        self.i += 1;
                        v -= self.term()?;
                    }
                    _ => return Some(v),
                }
            }
        }
        fn term(&mut self) -> Option<f64> {
            let mut v = self.atom()?;
            loop {
                self.ws();
                match self.b.get(self.i) {
                    Some(b'*') => {
                        self.i += 1;
                        v *= self.atom()?;
                    }
                    Some(b'/') => {
                        self.i += 1;
                        v /= self.atom()?;
                    }
                    _ => return Some(v),
                }
            }
        }
        fn atom(&mut self) -> Option<f64> {
            self.ws();
            match self.b.get(self.i)? {
                b'(' => {
                    self.i += 1;
                    let v = self.expr()?;
                    self.ws();
                    (self.b.get(self.i) == Some(&b')')).then(|| {
                        self.i += 1;
                        v
                    })
                }
                b'-' => {
                    self.i += 1;
                    Some(-self.atom()?)
                }
                _ => {
                    let start = self.i;
                    while self.i < self.b.len()
                        && (self.b[self.i].is_ascii_digit() || self.b[self.i] == b'.')
                    {
                        self.i += 1;
                    }
                    std::str::from_utf8(&self.b[start..self.i])
                        .ok()?
                        .parse()
                        .ok()
                }
            }
        }
    }
    let mut p = P {
        b: expr.as_bytes(),
        i: 0,
    };
    let v = p.expr()?;
    p.ws();
    (p.i == p.b.len()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_handles_precedence_and_parens() {
        assert_eq!(eval("2+3*4"), Some(14.0));
        assert_eq!(eval("(2+3)*4"), Some(20.0));
        assert_eq!(
            eval("120.5+89.99+310+45.25").map(|v| (v * 100.0).round() / 100.0),
            Some(565.74)
        );
        assert_eq!(eval("2+"), None);
    }
}
