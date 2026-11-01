use super::*;
use crate::automation::{
    self, WebhookAvailability, WebhookConnection, WebhookDelivery, WebhookEndpoint,
    WebhookSetupReceipt,
};

fn unavailable() -> OrchError {
    OrchError::NotAllowed("the webhook is unavailable".into())
}

pub(super) fn endpoint(
    tx: &ark_store::RecordTransaction<'_>,
    id: &str,
) -> Result<WebhookEndpoint, OrchError> {
    let endpoint: WebhookEndpoint = serde_json::from_value(
        tx.get_record(automation::WEBHOOK, id)?
            .ok_or_else(unavailable)?,
    )?;
    if endpoint.id != id {
        return Err(unavailable());
    }
    Ok(endpoint)
}

pub(super) fn setup_id(
    project: &str,
    automation: &str,
    request: &str,
) -> Result<String, OrchError> {
    if request.trim().is_empty() || request.len() > 256 || request.chars().any(char::is_control) {
        return Err(OrchError::NotAllowed(
            "a webhook setup needs a bounded request identity".into(),
        ));
    }
    Ok(format!(
        "hc-{}",
        blake3::hash(&serde_json::to_vec(&(project, automation, request))?).to_hex()
    ))
}

impl Orchestrator {
    pub fn webhook_connection_id(
        &self,
        automation: &str,
        request: &str,
    ) -> Result<String, OrchError> {
        setup_id(self.project().id.as_str(), automation, request)
    }

    pub fn configure_webhook(
        &self,
        automation: &str,
        expected: u64,
        request: &str,
        base: &str,
        max_per_hour: u32,
    ) -> Result<WebhookSetupReceipt, OrchError> {
        let connection_id = setup_id(self.project().id.as_str(), automation, request)?;
        let endpoint_id = format!(
            "wh-{}",
            blake3::hash(&serde_json::to_vec(&(
                self.project().id.as_str(),
                automation
            ))?)
            .to_hex()
        );
        if base.len() > 4096 {
            return Err(OrchError::NotAllowed(
                "the webhook address is too long".into(),
            ));
        }
        let base = url::Url::parse(base)
            .map_err(|_| OrchError::NotAllowed("enter a valid webhook base address".into()))?;
        let local = matches!(base.host(),Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(base.host(),Some(url::Host::Ipv6(ip)) if ip.is_loopback());
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
            || !(base.scheme() == "https" && base.port_or_known_default() == Some(443)
                || base.scheme() == "http" && local)
            || !(1..=1000).contains(&max_per_hour)
        {
            return Err(OrchError::NotAllowed("use an HTTPS base on port443 or this runtime's local listener, without credentials or another path".into()));
        }
        let url = base
            .join(&format!("webhooks/v1/{endpoint_id}"))
            .map_err(|_| unavailable())?
            .to_string();
        let digest = format!(
            "v1:{}",
            blake3::hash(&serde_json::to_vec(&(
                automation,
                expected,
                &url,
                max_per_hour
            ))?)
            .to_hex()
        );
        let receipt =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    if let Some(value) = tx.get_record(automation::WEBHOOK_SETUP, &connection_id)? {
                        let existing: WebhookSetupReceipt = serde_json::from_value(value)?;
                        if existing.digest != digest {
                            return Err(OrchError::NotAllowed(
                                "this setup request already names another connection".into(),
                            ));
                        }
                        return Ok::<_, OrchError>(existing);
                    }
                    let mut head: automation::Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, automation)?
                            .ok_or_else(unavailable)?,
                    )?;
                    let revision = automations::revision(tx, automation, head.revision)?;
                    if head.revision != expected
                        || !matches!(revision.draft.trigger, automation::Trigger::Webhook)
                    {
                        return Err(OrchError::NotAllowed(
                            "save the current webhook revision before configuring its connection"
                                .into(),
                        ));
                    }
                    let now = now_ms();
                    let connection = WebhookConnection {
                        id: connection_id.clone(),
                        url,
                        max_per_hour,
                        availability: WebhookAvailability::Pending,
                        verified_ms: None,
                    };
                    let mut record = match tx.get_record(automation::WEBHOOK, &endpoint_id)? {
                        Some(value) => serde_json::from_value::<WebhookEndpoint>(value)?,
                        None => WebhookEndpoint {
                            id: endpoint_id.clone(),
                            project: self.project().id.to_string(),
                            automation: automation.into(),
                            account: endpoint_id.clone(),
                            draft: connection.clone(),
                            active: None,
                            active_since_ms: 0,
                            enabled: false,
                            created_ms: now,
                            updated_ms: now,
                        },
                    };
                    if record.project != self.project().id.as_str()
                        || record.automation != automation
                    {
                        return Err(unavailable());
                    }
                    record.draft = connection.clone();
                    record.updated_ms = now;
                    head.webhook = Some(endpoint_id.clone());
                    head.updated_ms = now;
                    let receipt = WebhookSetupReceipt {
                        endpoint: endpoint_id.clone(),
                        request: request.into(),
                        digest,
                        connection,
                        created_ms: now,
                    };
                    tx.put_record(
                        automation::WEBHOOK,
                        &endpoint_id,
                        &serde_json::to_value(&record)?,
                    )?;
                    tx.put_record(
                        automation::DEFINITION,
                        automation,
                        &serde_json::to_value(head)?,
                    )?;
                    tx.insert_record(
                        automation::WEBHOOK_SETUP,
                        &connection_id,
                        &serde_json::to_value(&receipt)?,
                    )?;
                    Ok(receipt)
                })?;
        self.inner.emit(EventKind::Automation {
            automation: automation.into(),
            run: None,
        });
        Ok(receipt)
    }

    pub fn webhook_endpoint(&self, id: &str) -> Result<WebhookEndpoint, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| endpoint(tx, id))
    }

    pub fn webhook_route(
        store: &ark_store::Store,
        id: &str,
    ) -> Result<Option<WebhookEndpoint>, OrchError> {
        if id.len() != 67
            || !id.starts_with("wh-")
            || !id[3..].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(None);
        }
        let record = store
            .get_record(automation::WEBHOOK, id)?
            .map(serde_json::from_value::<WebhookEndpoint>)
            .transpose()?;
        if record.as_ref().is_some_and(|record| record.id != id) {
            return Err(unavailable());
        }
        Ok(record)
    }

    pub fn record_webhook_probe(
        &self,
        id: &str,
        connection: &str,
        local: bool,
    ) -> Result<WebhookEndpoint, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let mut record = endpoint(tx, id)?;
                if record.draft.id != connection {
                    return Err(OrchError::NotAllowed(
                        "the webhook connection changed before verification".into(),
                    ));
                }
                record.draft.availability = if local {
                    WebhookAvailability::Local
                } else {
                    WebhookAvailability::Verified
                };
                record.draft.verified_ms = Some(now_ms());
                record.updated_ms = now_ms();
                tx.put_record(automation::WEBHOOK, id, &serde_json::to_value(&record)?)?;
                Ok(record)
            })
    }

    pub fn webhook_deliveries(&self, id: &str) -> Result<Vec<WebhookDelivery>, OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                endpoint(tx, id)?;
                Ok(tx
                    .get_record(automation::WEBHOOK_LOG, id)?
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default())
            })
    }

    pub fn record_webhook_delivery(
        &self,
        id: &str,
        entry: WebhookDelivery,
    ) -> Result<(), OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                endpoint(tx, id)?;
                let mut log: Vec<WebhookDelivery> = tx
                    .get_record(automation::WEBHOOK_LOG, id)?
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                log.push(entry);
                if log.len() > 100 {
                    log.drain(..log.len() - 100);
                }
                tx.put_record(automation::WEBHOOK_LOG, id, &serde_json::to_value(log)?)?;
                Ok(())
            })
    }
}

impl Orchestrator {
    pub fn admit_webhook_event(
        &self,
        id: &str,
        connection: &str,
        event: &str,
        event_ms: u64,
        inputs: std::collections::BTreeMap<String, automation::InputValue>,
    ) -> Result<automation::WebhookAdmission, OrchError> {
        let (record, origin, prior, prepared) =
            self.inner
                .engine
                .store
                .transact_records(self.project().id.as_str(), |tx| {
                    let record = endpoint(tx, id)?;
                    if record
                        .active
                        .as_ref()
                        .is_none_or(|active| active.id != connection)
                    {
                        return Err(OrchError::NotAllowed(
                            "the verified webhook credential is no longer active".into(),
                        ));
                    }
                    let origin = automation::RunOrigin::Webhook {
                        endpoint: id.into(),
                        source: "webhook".into(),
                        account: record.account.clone(),
                        event: event.into(),
                    };
                    origin.validate().map_err(OrchError::NotAllowed)?;
                    let run_id = origin.run_id(self.project().id.as_str(), &record.automation)?;
                    let input_digest = origin.input_digest(&record.automation, 0, &inputs)?;
                    if let Some(value) = tx.get_record(automation::RUN, &run_id)? {
                        let run: automation::Run = serde_json::from_value(value)?;
                        if run.input_digest != input_digest {
                            return Err(OrchError::NotAllowed(
                                "this event identity already names another input".into(),
                            ));
                        }
                        return Ok::<_, OrchError>((record, origin, Some(run), None));
                    }
                    if event_ms <= record.active_since_ms {
                        return Err(OrchError::NotAllowed(
                            "this event predates the current activation".into(),
                        ));
                    }
                    let head: automation::Definition = serde_json::from_value(
                        tx.get_record(automation::DEFINITION, &record.automation)?
                            .ok_or_else(unavailable)?,
                    )?;
                    if !head.enabled || !record.enabled {
                        return Err(OrchError::NotAllowed("this webhook is paused".into()));
                    }
                    let grant: automations::PreparedActivation = serde_json::from_value(
                        tx.get_record(
                            automations::ACTIVATION,
                            head.activation.as_deref().ok_or_else(unavailable)?,
                        )?
                        .ok_or_else(unavailable)?,
                    )?;
                    if grant.value.phase != automation::ActivationPhase::Active {
                        return Err(OrchError::NotAllowed("the activation was revoked".into()));
                    }
                    Ok((record, origin, None, Some(grant)))
                })?;
        if let Some(run) = prior {
            return Ok(automation::WebhookAdmission {
                run,
                duplicate: true,
            });
        }
        let prepared = prepared.ok_or_else(unavailable)?;
        let input_digest =
            origin.input_digest(&record.automation, prepared.value.revision, &inputs)?;
        let (run, created) =
            self.inner
                .admit_frozen_automation_run_tracked(super::runs::FrozenRunAdmission {
                    automation: &record.automation,
                    expected: prepared.value.revision,
                    origin,
                    inputs,
                    input_digest,
                    prepared,
                    due_now: None,
                    authenticated_connection: Some((connection, event_ms)),
                })?;
        Ok(automation::WebhookAdmission {
            run,
            duplicate: !created,
        })
    }
}

impl Orchestrator {
    pub fn record_webhook_probe_failure(
        &self,
        id: &str,
        connection: &str,
    ) -> Result<(), OrchError> {
        self.inner
            .engine
            .store
            .transact_records(self.project().id.as_str(), |tx| {
                let mut record = endpoint(tx, id)?;
                if record.draft.id != connection {
                    return Err(OrchError::NotAllowed(
                        "the webhook connection changed before verification".into(),
                    ));
                }
                record.draft.availability = WebhookAvailability::Pending;
                record.draft.verified_ms = None;
                record.updated_ms = now_ms();
                tx.put_record(automation::WEBHOOK, id, &serde_json::to_value(record)?)?;
                Ok(())
            })
    }
}
