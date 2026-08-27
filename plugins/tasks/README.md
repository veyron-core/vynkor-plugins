# tasks plugin

Task CRUD as thin schema over `database`.

| Action | Params | Result |
|---|---|---|
| `task_create` | `{title, notes?, list?, due_ms?, tags?}` | `{id, task}` |
| `task_get` | `{id}` | `{found, task?}` |
| `task_list` | `{query?, list?, status?, tag?, limit?, offset?}` | `{tasks, total}` |
| `task_update` | `{id, title?, notes?, list?, due_ms?, tags?, done?}` | `{updated, task}` |
| `task_done` | `{id, done?}` | `{done, task}` |
| `task_delete` | `{id}` | `{deleted}` |

Publishes `plugin.tasks.changed` {op: created|updated|completed|reopened|deleted, id}.

## Config
`TASKS_PLUGIN_DB_TIMEOUT_MS` default 5000.
