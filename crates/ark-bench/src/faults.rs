//! Fault injection: a provider wrapper that fails some calls on a fixed schedule, so a
//! report says how a harness copes with a provider that misbehaves. The schedule comes from a
//! seed and the call's position, so two runs with the same seed see the same faults.
//!
//! | Fault | What the harness sees |
//! |---|---|
//! | `overloaded` | `ProviderError::Overloaded`: temporary, worth a retry |
//! | `malformed` | `ProviderError::Malformed`: a reply that cannot be used as sent |
//! | `bad-tool` | the reply's first tool call names a tool that does not exist |

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ark_sdk::prelude::ark_model::{
    Completion, Message, Provider, ProviderError, Result as ModelResult, TextSink, ToolDef, WebHit,
};
use async_trait::async_trait;

use crate::record::Recorder;

/// How often each fault happens, per model call (0.0 to 1.0).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FaultPlan {
    pub overloaded: f64,
    pub malformed: f64,
    pub bad_tool: f64,
    pub seed: u64,
}

impl FaultPlan {
    /// `overloaded=0.1,malformed=0.05,bad-tool=0.05`.
    pub fn parse(spec: &str, seed: u64) -> Result<FaultPlan, String> {
        let mut p = FaultPlan {
            seed,
            ..Default::default()
        };
        for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (k, v) = part
                .split_once('=')
                .ok_or_else(|| format!("`{part}`: expected name=rate"))?;
            let rate: f64 = v
                .parse()
                .map_err(|_| format!("`{part}`: the rate is not a number"))?;
            if !(0.0..=1.0).contains(&rate) {
                return Err(format!("`{part}`: the rate must be between 0 and 1"));
            }
            match k.trim() {
                "overloaded" => p.overloaded = rate,
                "malformed" => p.malformed = rate,
                "bad-tool" => p.bad_tool = rate,
                other => {
                    return Err(format!(
                        "unknown fault `{other}` (overloaded, malformed, bad-tool)"
                    ))
                }
            }
        }
        if p.overloaded + p.malformed + p.bad_tool > 1.0 {
            return Err("the rates add up to more than 1".into());
        }
        Ok(p)
    }

    /// The plan as it reads in a report: `overloaded=0.1,bad-tool=0.05 seed=7`.
    pub fn describe(&self) -> String {
        let parts: Vec<String> = [
            ("overloaded", self.overloaded),
            ("malformed", self.malformed),
            ("bad-tool", self.bad_tool),
        ]
        .iter()
        .filter(|(_, r)| *r > 0.0)
        .map(|(k, r)| format!("{k}={r}"))
        .collect();
        format!("{} seed={}", parts.join(","), self.seed)
    }
}

/// A provider that injects the faults of a [`FaultPlan`]. Each injected fault is counted on
/// the run's [`Recorder`].
pub struct Faulty {
    inner: Arc<dyn Provider>,
    plan: FaultPlan,
    calls: AtomicU64,
    recorder: Arc<Recorder>,
}

enum Fault {
    None,
    Overloaded,
    Malformed,
    BadTool,
}

impl Faulty {
    /// Wrap `inner`. `salt` varies the schedule between repetitions of one workload.
    pub fn wrap(
        inner: Arc<dyn Provider>,
        plan: &FaultPlan,
        salt: u64,
        recorder: Arc<Recorder>,
    ) -> Arc<dyn Provider> {
        let mut plan = plan.clone();
        plan.seed = plan
            .seed
            .wrapping_add(salt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        Arc::new(Faulty {
            inner,
            plan,
            calls: AtomicU64::new(0),
            recorder,
        })
    }

    fn next(&self) -> Fault {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let x = unit(splitmix(
            self.plan.seed ^ n.wrapping_mul(0xD1B5_4A32_D192_ED03),
        ));
        let p = &self.plan;
        if x < p.overloaded {
            Fault::Overloaded
        } else if x < p.overloaded + p.malformed {
            Fault::Malformed
        } else if x < p.overloaded + p.malformed + p.bad_tool {
            Fault::BadTool
        } else {
            Fault::None
        }
    }

    fn error(&self, fault: Fault) -> Option<ProviderError> {
        let provider = "fault injection".to_string();
        let detail = "injected by ark-bench".to_string();
        match fault {
            Fault::Overloaded => Some(ProviderError::Overloaded { provider, detail }),
            Fault::Malformed => Some(ProviderError::Malformed { provider, detail }),
            _ => None,
        }
    }

    /// Rename the first tool call to a tool that does not exist. A reply without tool calls
    /// is left alone (the fault did not apply, so it is not counted).
    fn bad_tool(&self, mut c: Completion) -> Completion {
        if let Some(call) = c.tool_calls.first_mut() {
            call.name.push_str("_v2");
            self.recorder.add_fault();
        }
        c
    }
}

#[async_trait]
impl Provider for Faulty {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ModelResult<Completion> {
        let fault = self.next();
        let bad = matches!(fault, Fault::BadTool);
        if let Some(e) = self.error(fault) {
            self.recorder.add_fault();
            return Err(e);
        }
        let c = self.inner.complete(system, messages, tools).await?;
        Ok(if bad { self.bad_tool(c) } else { c })
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> ModelResult<Completion> {
        let fault = self.next();
        let bad = matches!(fault, Fault::BadTool);
        if let Some(e) = self.error(fault) {
            self.recorder.add_fault();
            return Err(e);
        }
        let c = self
            .inner
            .complete_streaming(system, messages, tools, on_text)
            .await?;
        Ok(if bad { self.bad_tool(c) } else { c })
    }

    /// Searches go through unchanged; they are not model calls of the run.
    async fn web_search(&self, query: &str, limit: usize) -> Option<ModelResult<Vec<WebHit>>> {
        self.inner.web_search(query, limit).await
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn model_id(&self) -> String {
        self.inner.model_id()
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn unit(x: u64) -> f64 {
    (x >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_parse_and_describe() {
        let p = FaultPlan::parse("overloaded=0.2, bad-tool=0.1", 7).unwrap();
        assert_eq!(p.overloaded, 0.2);
        assert_eq!(p.bad_tool, 0.1);
        assert_eq!(p.describe(), "overloaded=0.2,bad-tool=0.1 seed=7");
        assert!(FaultPlan::parse("slow=0.1", 1).is_err());
        assert!(FaultPlan::parse("overloaded=0.7,malformed=0.5", 1).is_err());
    }

    #[test]
    fn the_schedule_depends_only_on_the_seed() {
        let rec = Recorder::new();
        let mock: Arc<dyn Provider> = Arc::new(
            ark_sdk::prelude::ark_model::MockProvider::new("m", vec![])
                .with_fallback(|_| Completion::default()),
        );
        let plan = FaultPlan::parse("overloaded=0.5", 3).unwrap();
        let pattern = |p: &Arc<dyn Provider>| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            (0..40)
                .map(|_| rt.block_on(p.complete("s", &[], &[])).is_err())
                .collect::<Vec<_>>()
        };
        let a = pattern(&Faulty::wrap(mock.clone(), &plan, 0, rec.clone()));
        let b = pattern(&Faulty::wrap(mock.clone(), &plan, 0, rec.clone()));
        let c = pattern(&Faulty::wrap(mock, &plan, 1, rec));
        assert_eq!(a, b);
        assert_ne!(a, c);
        let failed = a.iter().filter(|x| **x).count();
        assert!((10..=30).contains(&failed), "{failed} of 40");
    }
}
