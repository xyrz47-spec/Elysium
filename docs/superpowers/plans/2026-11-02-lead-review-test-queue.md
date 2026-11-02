# Lead Review, Test Queue, Worker Windows and Groups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let blocked Gemini workers run commands through lead + JEV review, queue game-window tests app-wide, let the user and the lead talk mid-task (also from the phone), widen the right panel, pop workers out into OS windows, and let workers cooperate in groups.

**Architecture:** Pure policy and protocol parsing live in `lead.rs`; the app-wide FIFO lives in `testqueue.rs`; async glue (`TaskLead`, `run_worker`, `run_group`) lives in `leadrun.rs` and is called from `run_team_work`. Workers talk to Elysium through fenced blocks at the end of a turn, and Elysium resumes the conversation with results. The UI consumes structured events only, so backend code carries no UI copy.

**Tech Stack:** Rust, Tauri 2, tokio, React, TypeScript.

**Spec:** `docs/superpowers/specs/2026-11-02-lead-review-test-queue-design.md`

## Global Constraints

- UI copy goes through `__AGY__("Context: ... | Intent: ... | Tone: ... | Constraints: ... | Target Language: Korean")` and `just-code-claude --replace-in-file <path> -m "3.8 flash" -e medium`. Keep new copy in new files so the replacement scans small files. Prompts to AIs are not UI copy.
- `testSlots`: default 1, range 1-8. Panel default width 560px.
- Chain characters that void a similar-command match: `;`, `|`, `&`, `>`, backtick, `$(`, newline.
- Patterns refused for "similar": `powershell`, `pwsh`, `cmd`, `bash`, `sh`, `python`, `node`, `iex`, `Invoke-Expression`, `Start-Process` as a bare program.
- Existing line endings stay: `orchestrator.rs`, `engines.rs`, `intent.rs` are CRLF, `teamwork.rs` is LF.
- No commits unless the user asks. Do not restart the installed Elysium app.

## Spike Results (2026-11-02)

- `agy --input-format stream-json --conversation <id>` resumes and keeps context; a worker told the format emits an `elysium-request` block. Confirmed.
- Gradle daemon child (`ProcessBuilder` from a task, parent `java.exe` GradleDaemon) was gone 4 seconds after `taskkill /T /F` on the client tree. So `--no-daemon` is not added. Real game window still to be observed if the user runs one.

## Values Chosen (spec left to builder)

| Name | Value |
|---|---|
| Worker turn cap | 40 |
| Group round cap | 12 |
| `timeout_sec` command | default 600, clamp 5..=3600 |
| `timeout_sec` window | default 1800, clamp 60..=7200 |
| JEV safe | safe >= 0.85 and scope >= 0.7, else lead |
| JEV window | >= 0.5 |
| JEV manual | >= 0.6 routes to lead even under allow-all |
| JEV blocked or off plan | >= 0.6 calls lead |
| Lead call timeout | 300s |
| Panel width | min 360, max = window width - 152 - 420 |
| Stream keep | 300,000 chars, activities 200 kept, panel shows last 6 |

## Review Focus

- Worker asks for the same command twice while the first review is in flight: the second must see the rule the first created.
- Stop pressed while a worker is between turns (waiting on review, queue or question): nothing may keep running and the wait must end.
- Message sent to a worker between turns: must arrive on its next turn, not fail with "already finished".
- Window narrower than panel plus chat minimum: panel shrinks instead of covering the chat or overflowing.
- Worker reply with a broken block, or a block plus normal prose: prose survives, block never reaches the user or integrator.

---

### Task 1: Policy and protocol parsing

**Files:**
- Create: `src-tauri/src/lead.rs`
- Modify: `src-tauri/src/lib.rs` (register module)

**Interfaces:** Produces:
- `WorkerRequest { command: String, why: String, kind: Kind, timeout_sec: Option<u64> }`, `enum Kind { Command, Window }`
- `parse_requests(text: &str) -> Parsed { requests: Vec<WorkerRequest>, clean: String, error: Option<String> }`
- `GroupNote { say: String, claim: Vec<String>, done: bool }`, `parse_group(text: &str) -> (Option<GroupNote>, String)`
- `has_chain(cmd: &str) -> bool`, `normalize(cmd: &str) -> Vec<String>`, `pattern_allowed(p: &str) -> bool`
- `Policy { allow_all: bool, rules: Vec<Rule> }`, `Rule { pattern: String, window: bool }`, `Policy::matches(&self, cmd: &str) -> Option<Matched>`, `enum Matched { All, Rule(String, bool) }`, `Policy::add_similar(&mut self, pattern: &str, window: bool) -> bool`
- `LeadDecision { decision: Verdict, pattern: Option<String>, window: bool, reason: String, question: Option<String>, options: Vec<String>, workers: Vec<usize> }`, `enum Verdict { Once, Deny, Similar, All, AskUser }`, `parse_decision(text: &str) -> Result<LeadDecision, String>`
- `LeadActions { reply: Option<String>, messages: Vec<(usize, String)>, stop: Vec<usize>, ask_user: Option<Asked> }`, `Asked { question, options, workers }`, `parse_actions(text: &str) -> Result<LeadActions, String>`
- `claim_overlaps(notes: &[(usize, &GroupNote)]) -> Vec<usize>` (indexes of later claimers)
- `clamp_timeout(kind: Kind, asked: Option<u64>) -> u64`
- `REQUEST_SYSTEM: &str`, `GROUP_SYSTEM: &str`, `review_prompt(...)`, `actions_prompt(...)`

- [ ] **Step 1: Write failing tests** `requests_are_parsed_and_stripped`, `broken_block_keeps_prose_and_reports_error`, `chain_characters_void_a_similar_rule`, `normalize_treats_dot_slash_and_extensions_alike`, `bare_interpreters_cannot_become_patterns`, `allow_all_matches_everything`, `decision_with_bad_json_is_an_error`, `similar_without_pattern_is_an_error`, `actions_parse_messages_and_questions`, `later_claimer_of_a_file_is_held`, `timeouts_clamp_by_kind`. Key assertions:

```rust
let p = parse_requests("done.\n```elysium-request\n{\"requests\":[{\"command\":\".\\\\gradlew.bat test\",\"why\":\"w\",\"kind\":\"window\"}]}\n```");
assert_eq!(p.clean.trim(), "done."); assert_eq!(p.requests[0].kind, Kind::Window);
let bad = parse_requests("note\n```elysium-request\n{oops\n```");
assert_eq!(bad.clean.trim(), "note"); assert!(bad.error.is_some() && bad.requests.is_empty());
let mut po = Policy::default(); assert!(po.add_similar("./gradlew.bat :a:test", false));
assert!(po.matches(".\\gradlew :a:test --info").is_some());
assert!(po.matches("./gradlew :a:test; rm x").is_none());
assert!(po.matches("./gradlew :b:test").is_none());
assert!(!pattern_allowed("powershell")); assert!(!pattern_allowed("python")); assert!(pattern_allowed("python tools/x.py"));
assert_eq!(clamp_timeout(Kind::Window, None), 1800); assert_eq!(clamp_timeout(Kind::Command, Some(99999)), 3600);
```

- [ ] **Step 2: Run `cargo test --lib lead::` from `src-tauri`.** Expected: compile failure.
- [ ] **Step 3: Implement.** Token-prefix match on `normalize`; `matches` returns None whenever `has_chain` is true unless `allow_all`. Decision JSON: take the first `{`..last `}` so prose around JSON survives. `similar` without a valid pattern downgrades to `Once`; `pattern_allowed` false downgrades too.
- [ ] **Step 4: Run the tests.** Expected: PASS.

### Task 2: App-wide test queue

**Files:** Create `src-tauri/src/testqueue.rs`; Modify `settings.rs`, `src/lib/api.ts` (`testSlots: number`), `lib.rs`, `orchestrator.rs` (`AppState.tests: Arc<TestQueue>`), `commands.rs` (`settings_update` calls `set_capacity`).

**Interfaces:** Produces:
- `TestQueue::new(capacity: usize)`, `set_capacity(&self, n: usize)`, `async acquire(self: &Arc<Self>, item: TestItem, cancel: &CancellationToken) -> Option<TestGuard>`, `position(&self, id: u64) -> Option<usize>`, `snapshot(&self) -> QueueView`, `next_id(&self) -> u64`, `set_on_change(&self, f)`
- `TestItem { id: u64, task: String, key: String, command: String }`, `QueueView { capacity, running: Vec<TestItem>, waiting: Vec<TestItem> }`
- `cancel_test(&self, id: u64) -> bool` cancels a registered per-test token.
- `TestGuard` releases its slot on drop.

- [ ] **Step 1: Write failing tokio tests** `fifo_order_with_one_slot`, `raising_capacity_starts_the_next_waiter`, `lowering_capacity_keeps_running_tests`, `cancel_while_waiting_leaves_the_queue`, `guard_drop_releases_slot`, and in `settings.rs` `test_slots_default_and_clamp` (`Settings::default().test_slots == 1`, 0 becomes 1, 99 becomes 8, old JSON without the field gives 1).
- [ ] **Step 2: Run.** Expected: compile failure.
- [ ] **Step 3: Implement** with `Mutex<State>` and `tokio::sync::Notify`; a waiter proceeds only when it is the queue head and `running < capacity`.
- [ ] **Step 4: Run.** Expected: PASS.

### Task 3: Worker loop, review and execution

**Files:** Create `src-tauri/src/leadrun.rs`; Modify `api_agent.rs` (`pub(crate) powershell`), `orchestrator.rs` (`AppState.leads`, `agent_stops`, `agent_inboxes`; `cancel_agent` and `message_agent` consult them; JEV functions `judge_command`, `judge_turn`).

**Interfaces:**
- Consumes: Tasks 1 and 2, `run_engine_as`, `engines::RunSpec`, `jev::ask`, `jev::prob`.
- Produces: `TaskLead` (owned strings, `engine`, `model`, `session: Mutex<Option<String>>`, `policy`, `review: tokio::Mutex<()>`, `calls: tokio::Mutex<()>`, `decisions`, `workers: Mutex<Vec<WorkerInfo>>`, `holds`); `WorkerInfo { key, title, group: Option<usize> }`; `CommandJudgment { safe, in_scope, window, manual }`; `async run_worker(ctx: &Ctx, spec: RunSpec, key: &str, budget: Duration) -> Outcome`; `async decide(ctx, key, req) -> Ruling`; `async exec_now(...)`; `spawn_window_test(...) -> PendingTest`; `async ask_user(ctx, Asked) -> Option<String>`; `async consult_lead(ctx, why) -> ()`; `Ctx { app: AppHandle, st: Arc<AppState>, lead: Arc<TaskLead>, cancel: CancellationToken }`.
- Events: `lead:decision`, `lead:policy`, `lead:question`, `lead:question-closed`, `lead:said`, `task:agent-wait { taskId, key, note }`, `queue:changed`.

- [ ] **Step 1: Write failing tests** for the pure helpers: `results_prompt_lists_each_command`, `queued_result_tells_worker_to_continue_or_end_turn`, `merged_outcome_sums_usage_and_keeps_last_text`, `stop_during_wait_ends_the_loop` (a `tokio::test` around `wait_or_stop`).
- [ ] **Step 2: Run.** Expected: compile failure.
- [ ] **Step 3: Implement `decide`.** Order: JEV judgment, window flag, policy match (manual >= 0.6 skips auto-allow), then under `review` lock re-check policy, JEV safe path yields `Once`, otherwise lead with JEV opinion in the prompt. `AskUser` releases the lock, holds `workers`, waits, then re-asks the lead with the answer. Failure or unparsable answer denies. Every outcome records `lead:decision`, `audit::record`, and `decisions`.
- [ ] **Step 4: Implement `run_worker`.** Each turn: `run_engine_as` with `resume`; `parse_requests`; broken block returns an error prompt; command requests run inline racing stop; window requests go to `spawn_window_test` and reply with position; no requests with pending tests awaits them; no requests and none pending runs `judge_turn` then ends. Drain the inbox into the next prompt. Turn cap 40. Queue wait and test time never count against `budget`. Between turns emit `task:agent-wait`.
- [ ] **Step 5: Run `cargo test --lib`.** Expected: PASS.
- [ ] **Step 6: Live check** with a temporary test: real `agy`, lead `claude` haiku, in a scratch folder. Worker requests `git --version`, gets the result, finishes. Remove the temporary test.

### Task 4: Wire into team work, lead talk, phone answers

**Files:** Modify `orchestrator.rs` (`run_team_work`, `run_task` integration through `run_worker` with no reviewer for window requests), `teamwork.rs` (`Part.members`, split prompt asks for groups and says written groups must be followed, `parse_parts` caps total members), `engines.rs` (Claude `--disallowedTools` window patterns; verify live), `commands.rs` + `lib.rs` (`lead_send`, `lead_answer`, `lead_revoke`, `test_cancel`, `queue_get`, `lead_questions`), `remote.rs` (`GET /api/questions`, `POST /api/questions/{id}`), `mobile/src/remote.ts`, `mobile/src/App.tsx`.

**Interfaces:** Consumes Task 3. Produces commands `lead_send(task_id, text, about_key: Option<String>)`, `lead_answer(question_id, text)`, `lead_revoke(task_id)`, `test_cancel(id)`, `queue_get()`; `Part.members: usize` (serde default 1); `orchestrator::answer_question(app, st, id, text, actor) -> AppResult<()>`.

- [ ] **Step 1: Write failing tests** `parts_read_members_and_cap_total` (`[{members:3},{members:3}]` with n=4 keeps every part at least 1 and total 4), `split_prompt_demands_written_groups`, `claude_args_block_known_window_tasks` (work mode only), `first_answer_wins_and_second_is_an_error`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** Antigravity and other resumable CLI workers go through `run_worker`; non-resumable engines keep `run_engine_as` plus the system note. Add `decisions` to `team_json`. Lead gets the split `session_id`. `JEV` checks written groups are honoured (>= 0.6 mismatch re-splits once).
- [ ] **Step 4: Run `cargo test --lib`.** Expected: PASS.

### Task 5: Worker groups

**Files:** Modify `leadrun.rs` (`run_group`), `orchestrator.rs` (members per part, shared `dirs[i]`, `judge_group`), events `task:groups`, `group:say`.

**Interfaces:** `async run_group(ctx, title, members: Vec<(String, RunSpec)>, budget) -> Outcome`, `group_followup(round, others) -> String`. Consumes `run_worker`, `parse_group`, `claim_overlaps`.

- [ ] **Step 1: Write failing tests** `group_followup_carries_others_and_cuts_long_notes`, `held_member_skips_the_round`, `round_cap_stops_the_group`, `failed_member_does_not_end_the_group` (loop logic extracted into a pure `next_round(state) -> RoundPlan`).
- [ ] **Step 2: Run.** Expected: compile failure.
- [ ] **Step 3: Implement.** Round 0 plans without editing; later rounds run members concurrently; stop when all `done` or at cap 12; JEV blocked calls the lead; first surviving member writes the summary. Non-resumable members receive the full log.
- [ ] **Step 4: Run tests and a live check** with two `agy` members in a scratch folder. Expected: both edit different files and read each other's notes. Remove the temporary test.

### Task 6: Panel, worker windows, close handling

**Files:** Create `src/components/LeadPanel.tsx`, `src/WorkerWindow.tsx`, `src/lib/panel.ts`; Modify `src/components/Tools.tsx`, `src/store.tsx`, `src/main.tsx`, `src/App.tsx`, `src/styles.css`, `src/lib/api.ts`, `src/lib/theme.ts` (`storage` listener), `src/components/SettingsDialog.tsx`, `src-tauri/capabilities/default.json` (`worker-*`), `commands.rs` (`worker_window_open` async), `lib.rs` (main close cancels tasks, then exits).

**Interfaces:** `clampPanel(stored: number, windowWidth: number): number` in `panel.ts`. Rust `worker_window_open(app, task_id, key, title)` with label `worker-<task8>-<key>`; same label focuses the existing window. URL query `?window=worker&task=&key=`. The window asks `worker-window:hello`, the main store answers `worker-window:snapshot`.

- [ ] **Step 1: Write the pure `clampPanel` with an assertion script.** `clampPanel(560,1280) === 560`, `clampPanel(560,900) === 360` (min wins), `clampPanel(900,1280) === 708`.
- [ ] **Step 2: Implement store and panel.** Keep 300,000 chars and 200 activities; a restarted agent keeps its history and start time. Grid column 3 widens by panel width via `--tools-w`. Drag handle persists to `elysium.toolsWidth` and re-clamps on resize.
- [ ] **Step 3: Implement LeadPanel.** Lead card (state, question, full-instruction box), worker and group cards (two columns, alert lead, stop, open window), queue card (slots, per-test stop), decision log (revoke button). Use `__AGY__` placeholders and run the replacement.
- [ ] **Step 4: Implement WorkerWindow** with `ParticleOrb`, same CSS and theme, output, activities, files, request state, alert lead, stop.
- [ ] **Step 5: Add settings field** for `testSlots` and show saved decisions and group logs in message history.
- [ ] **Step 6: Run** `npx tsc --noEmit`, `npm run build`, `cargo test --lib`. Expected: all pass.

### Task 7: Final verification

- [ ] **Step 1: Run** `cargo build` and a full frontend build from the repo.
- [ ] **Step 2: Launch a dev build** and exercise panel resize, a worker window and theme change. Report anything not exercised.
- [ ] **Step 3: One fresh reviewer** on the whole diff, then fix verified findings.
