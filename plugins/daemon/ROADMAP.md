# daemon roadmap

Scope notes and non-goals for the headless voice client. Root picture lives
in the repo's `ROADMAP.md`; this file follows the per-plugin pattern
(`plugins/ai/ROADMAP.md`).

## Non-goals

- **No business logic.** The daemon is a thin client to `agent`, same
  relationship `notes` has to `database`. Prompt shaping, tool gating, goal
  state — all live in `agent`. If a feature request starts with "the daemon
  should decide…", it belongs in `agent`.
- **No wake-word / VAD in v0.1.** The capture window is a fixed timer; the
  daemon never sees the PCM (it flows mic → stt directly), so there is no
  audio to detect silence on client-side. A VAD turn-end would need either
  stt publishing partial transcripts or mic exposing level events — both are
  upstream features.
- **No direct peer streaming.** All calls are kernel-routed actions; the
  daemon holds no `PERMISSION_IPC_SEND` and no `ipc_targets`.
- **No per-call tts overrides on `daemon_say` in v0.1.** Provider/voice/
  format are operator config; callers needing variety should call `tts`
  themselves.
- **No confirmation handling.** `needs_confirmation` goals are reported,
  never resumed (`daemon_ask` returns them; approval goes through `agent`'s
  `goal_resume`). A voice-driven "yes, do it" flow wants an explicit
  allowlisted confirmation utterance design first.

## Open options

- Migrate the turn trigger to stt partial-transcript events (real-time
  turn-taking) once stt grows them.
- An `daemon_ask_stream` that relays agent steps as events for clients that
  want progress narration.
- Per-goal `context`/profile passthrough (`DAEMON_PLUGIN_AI_AGENT_ID`-style)
  if a second deployment persona shows up.

## Shipped

- v0.1.0 — six actions (`enable/disable/status/turn/say/ask`), opt-in
  background loop, single busy slot, `turn.completed`/`state.changed`
  events, fake-kernel e2e suite (23 tests).
