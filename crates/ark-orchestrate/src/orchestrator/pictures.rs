//! The pictures a person attaches to a message (ADR 0121): read and fingerprinted when the
//! message is admitted, read again and compared when the turn starts.

use super::*;
use crate::record::TaskImage;

impl Inner {
    /// What was admitted for `spec`, or why it is refused. Pictures belong to a message the
    /// person sent from their own chat: delegation, `agents.start`, schedules, automations,
    /// group turns and results posted into a chat refuse them.
    pub(super) fn admit_images(
        &self,
        spec: &TaskSpec,
        origin: &Origin,
    ) -> Result<Vec<TaskImage>, OrchError> {
        if spec.images.is_empty() {
            return Ok(Vec::new());
        }
        let from_person = matches!(origin, Origin::External)
            && spec.person
            && !spec.group_turn
            && !spec.reply
            && spec.report_to.is_none()
            && spec.work.is_none();
        if !from_person {
            return Err(OrchError::NotAllowed(
                "pictures go only with a message the person sends".into(),
            ));
        }
        let root = self.engine.workspace_dir(&self.project.id)?;
        let read = ark_files::images(&root, &spec.images)
            .map_err(|error| OrchError::NotAllowed(error.to_string()))?;
        Ok(read
            .into_iter()
            .map(|image| TaskImage {
                path: image.path,
                sha256: image.sha256,
                size: image.size,
            })
            .collect())
    }

    /// The admitted pictures as the model is shown them. A file that is gone or is another file
    /// since admission fails the turn, naming it.
    pub(super) fn start_pictures(
        &self,
        admitted: &[TaskImage],
    ) -> Result<Vec<ark_model::Picture>, String> {
        if admitted.is_empty() {
            return Ok(Vec::new());
        }
        let root = self
            .engine
            .workspace_dir(&self.project.id)
            .map_err(|error| error.to_string())?;
        admitted
            .iter()
            .map(|was| {
                let changed = || {
                    format!(
                        "The picture {} changed or was removed after it was sent.",
                        was.path
                    )
                };
                let [now] = ark_files::images(&root, std::slice::from_ref(&was.path))
                    .map_err(|_| changed())?
                    .try_into()
                    .map_err(|_| changed())?;
                if now.sha256 != was.sha256 || now.size != was.size {
                    return Err(changed());
                }
                Ok(ark_model::Picture {
                    media_type: now.media_type,
                    bytes: Arc::new(now.bytes),
                })
            })
            .collect()
    }
}

impl Orchestrator {
    /// The pictures of an admitted General message, checked as a turn start checks them.
    pub fn general_work_pictures(
        &self,
        receipt: &str,
    ) -> Result<(Vec<String>, Vec<ark_model::Picture>), OrchError> {
        let admitted = self.inner.engine.store.transact_records(
            self.project().id.as_str(),
            |tx| -> Result<Vec<TaskImage>, OrchError> {
                let value = tx
                    .get_record(admission::SUBMISSION, receipt)?
                    .ok_or_else(|| {
                        OrchError::NotAllowed("the admitted message is unavailable".into())
                    })?;
                Ok(serde_json::from_value::<admission::Submission>(value)?.images)
            },
        )?;
        let pictures = self
            .inner
            .start_pictures(&admitted)
            .map_err(OrchError::NotAllowed)?;
        Ok((admitted.into_iter().map(|i| i.path).collect(), pictures))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pair() -> (tempfile::TempDir, Orchestrator, String) {
        let dir = tempfile::tempdir().unwrap();
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(Arc::new(ark_model::MockProvider::new("test", vec![])))
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("team", "test").unwrap();
        let o = Orchestrator::builder(&ark, &project).build().unwrap();
        o.spawn(AgentSpec::new("lead").delegates_to(["w"]))
            .await
            .unwrap();
        o.spawn(AgentSpec::new("w")).await.unwrap();
        let task = o
            .inner
            .create_task("lead", TaskSpec::new("lead's job"), Origin::External, None)
            .unwrap();
        (dir, o, task)
    }

    fn with_pictures() -> TaskSpec {
        let mut spec = TaskSpec::new("look").images(["shot.png"]);
        spec.person = true;
        spec
    }

    #[tokio::test]
    async fn delegation_and_agents_start_refuse_pictures_even_with_a_person_flag() {
        let (_dir, o, task) = pair().await;
        let delegated = o.inner.delegate("lead", &task, "w", with_pictures());
        assert!(delegated
            .unwrap_err()
            .to_string()
            .contains("only with a message the person sends"),);
        let started = o
            .inner
            .start_conversation("lead", &task, "w", with_pictures())
            .await;
        assert!(started
            .unwrap_err()
            .to_string()
            .contains("only with a message the person sends"),);
        assert_eq!(o.tasks().len(), 1, "only the lead's own task exists");
    }
}
