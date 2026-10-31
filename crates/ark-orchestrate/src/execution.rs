use serde::Serialize;
use serde_json::Value;

use crate::OrchError;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ExecutionSelection {
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

impl ExecutionSelection {
    pub fn parse(input: &Value) -> Result<Self, OrchError> {
        let field = |key: &str| -> Result<Option<String>, OrchError> {
            input
                .get(key)
                .map(|value| {
                    value
                        .as_str()
                        .filter(|s| !s.is_empty() && s.trim() == *s && s.len() <= 512)
                        .map(str::to_string)
                        .ok_or_else(|| {
                            OrchError::NotAllowed(format!("{key} must be a non-empty string"))
                        })
                })
                .transpose()
        };
        let selection = Self {
            model: field("model")?,
            reasoning: field("reasoning")?,
        };
        if selection.reasoning.is_some()
            && selection.model.as_ref().is_some_and(|s| s.contains('@'))
        {
            return Err(OrchError::NotAllowed(
                "choose reasoning separately or in model, not both".into(),
            ));
        }
        Ok(selection)
    }

    pub fn apply(&self, inherited: Option<&str>) -> Result<Option<String>, OrchError> {
        let model = self.model.as_deref().or(inherited);
        match (&self.reasoning, model) {
            (Some(effort), Some(model)) => {
                if !effort.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    return Err(OrchError::NotAllowed("invalid reasoning effort".into()));
                }
                Ok(Some(format!(
                    "{}@{effort}",
                    model.split('@').next().unwrap_or(model)
                )))
            }
            (Some(_), None) => Err(OrchError::NotAllowed(
                "choose a model before reasoning".into(),
            )),
            (None, model) => Ok(model.map(str::to_string)),
        }
    }

    pub fn explicit(&self) -> bool {
        self.model.is_some() || self.reasoning.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn optional_execution_preserves_account_and_resets_effort_only_when_selected() {
        let parent = Some("provider-a~account:large@high");
        assert_eq!(
            ExecutionSelection::default()
                .apply(parent)
                .unwrap()
                .as_deref(),
            parent
        );
        assert_eq!(
            ExecutionSelection::parse(&json!({"reasoning":"low"}))
                .unwrap()
                .apply(parent)
                .unwrap()
                .as_deref(),
            Some("provider-a~account:large@low")
        );
        assert_eq!(
            ExecutionSelection::parse(&json!({"model":"provider-b:small"}))
                .unwrap()
                .apply(parent)
                .unwrap()
                .as_deref(),
            Some("provider-b:small")
        );
    }

    #[test]
    fn malformed_or_ambiguous_execution_is_refused() {
        for input in [
            json!({"model":null}),
            json!({"model":""}),
            json!({"model":3}),
            json!({"reasoning":false}),
            json!({"model":"provider-a:a@high","reasoning":"low"}),
        ] {
            assert!(ExecutionSelection::parse(&input).is_err(), "{input}");
        }
        assert!(ExecutionSelection::parse(&json!({"reasoning":"high"}))
            .unwrap()
            .apply(None)
            .is_err());
        assert!(ExecutionSelection::parse(&json!({"reasoning":"high@bad"}))
            .unwrap()
            .apply(Some("provider-a:a"))
            .is_err());
    }
}
