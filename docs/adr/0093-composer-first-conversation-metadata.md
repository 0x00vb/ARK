# 0093. A conversation starts with the request

Status: Accepted for implementation
Date: 2026-10-31
Amends: 0092

## Decision

A consumer may open a project conversation directly in the main column. Its shared chat view
owns the composer, model selection, optional reasoning and file origin. A resourceScoped view receives
a captured API with its authorized opening chat. Changing runtime or project disposes that
controller. Existing projectScoped views keep their live API and project-update lifecycle. The title, title actions,
keyboard focus and drop capability come from that controller. General session history remains
accessible. This does not generalize the session split or detached-window protocol to agents.

AgentSpec title and description are display metadata. Missing values default to empty. The
stable name remains the routing key. Renaming display metadata preserves scope, task ownership
and creation receipts. Neither field is inserted into agent_profile. Role and instructions keep
their explicit behavioral meaning. Graph nodes and read-only contacts project display metadata;
older clients can keep using the stable name.

Desktop opens a local draft on New chat. The first request supplies the initial title and
short description without another model call. First Send freezes the creation and submission
inputs. Creation and submission use separate exact retry receipts. If either reply is lost,
Retry uses those frozen inputs. Later typing and upload completion remain separate from the
attempt. New chat does not silently replay an uncertain request. Pending drafts can be reopened
from the conversation menu in the same runtime and project. Draft recovery is window-local;
closing the application before a receipt is recovered is not qualified crash recovery.

The optional submit_task request_id accepts only a task objective and optional context. Other
TaskSpec fields are refused in this mode. The existing submit_once compares all supported
inputs. The ordinary full TaskSpec endpoint without request_id is unchanged.

## Consequences

Empty drafts create no agent or task. The default agent chat no longer leaves an empty general
chat beside it. Model selection does not change the project default. A failed first send has a
visible Retry action; editing the composer cannot mutate an uncertain request. Display metadata
can be edited after acknowledgment, without turning the first request into permanent behavior.
