# External clients create projects and agents

Status: accepted, 31 October2026.

The native client needs the same named scratch projects and idle agents as Desktop. Protocol1.10 adds project.create and agent.create to the existing device epoch/counter/digest ledger. Inputs contain only a project name, or a project ID, agent name and brief role. Receipts may identify the created resource. No arbitrary RPC or paths.

Projects use a stable ID derived from the accepted ledger key. Top-level names are unique, enforced atomically by Store.put_project for every entry point. Existing stores remain readable; conflicts do not rename or delete anything. Children may share names.

Agent creation uses Orchestrator.create_once with creation_request provenance in the same saved AgentRecord. The normal creation lock also covers Desktop changes. Retries return only matching provenance without overwriting later edits. Unrelated same-name agents conflict. No task starts. The blank spec follows Desktop defaults and existing effective profile/tool limits; client inputs cannot change permissions.

Agent metadata optionally includes read-only schedules using shared server display. The native client does not interpret cron syntax. Older readers ignore additive data, and minimum client minor stays0.
