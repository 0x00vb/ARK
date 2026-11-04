# 0096: Open the assigned agent conversation from Work

Status: accepted, local candidate. Date: 2026-11-03.

Work owns admission and execution receipts. A consumer owns its agent conversation view.
The receipt reporting conversation and the agent's sidebar conversation can differ.

A view can declare `agentConversation: true`. The shared `openAgentChat(name, conversation)`
uses that view through the location-scoped view API, or opens the receipt conversation when
no consumer view is registered. Core does not name a consumer view. Work uses the current
owner and retains admission and creation identities through retry. An unassigned send creates
an agent with the existing safe defaults, assigns the item, and opens its agent conversation.

This adds no backend authority, transcript copying or consumer dependency to Core.
