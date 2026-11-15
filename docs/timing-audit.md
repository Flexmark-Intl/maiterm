# Timing audit (2026-11-15)

A plan and goal brief for taking timing out of maiTerm's Rust code: every place in
`src-tauri/src` that sleeps, times out, ticks, or decides something by comparing a clock against
a threshold, what each one should become, and how a Hive can do the work in parallel. Nothing
here is fixed yet. `src-tauri/src/filetree/` is left out: its tests are being fixed separately
(§2.2).

Read §1 and §2 first. §3 is the inventory, §4 the goal, §5 the waves, §6 the traps that cost
earlier workers time.

## 1. The rule

Darryl, 2026-11-15: "if we're relying on things like x seconds timers then we're probably
building something wrong ... ALL timing type coding is flaky and should be avoided whenever
possible."

In practice:

- **Wait on something that happens**: an event, a channel message, an ack, a condition variable,
  a sequence number, a process exit. Never `sleep(n)` followed by an assertion or a decision.
- **Never prove a non-event by sleeping.** "Sleep 400 ms, then check nothing ran" passes on a
  slow machine for the wrong reason and fails on a loaded one. Make the code report the decision
  it made ("skipped: repo is slow", "held: draft in the box") and assert on that report.
- **Where time genuinely is the behaviour** (debounce, slow-mode detection, kill-after-timeout,
  retry backoff, idle stop, TTL, a periodic tick that is the feature), keep it, but put the clock
  or the durations behind something a test can feed. The test then hands in "this run took
  400 ms" or "an hour has passed" instead of waiting for it.
- **A watchdog or deadline is acceptable only to turn a hang into a clear failure**, never to make
  something pass. A deadline whose expiry is treated as success, or falls back silently to
  something else, is not a watchdog: it is a guess.
- **A "timing flake" is a defect.** Don't rerun it and move on; find what the code should wait on.

## 2. Why: two real cases

### 2.1 The Windows CI hang

The Test workflow (`.github/workflows/test.yml`) runs `cargo test --locked` on `windows-latest`.
On 2026-11-13 it hung in `filetree::ops::tests::an_op_through_a_subscription_shows_up_on_the_channel`
until GitHub's six-hour default, and again on 2026-11-14. The pushes that hung touched no
filetree code, so each one looked like a failure of whatever had just been pushed.

The cause, as best established (it was never reproduced): notify 7's Windows watcher stops a
watch by closing the directory handle and then waiting for the read's completion routine. A read
that had already completed re-armed `ReadDirectoryChangesW` on the closed handle's value, which
Windows had already handed to the next handle the process opened. The test deletes its temp
folder right after dropping the watcher, and `remove_dir_all` opens synchronous directory
handles, so the two collided.

The fix (commits 8e8b90c5 and 97969324) replaced notify's Windows watcher with
`filetree/watch/windows.rs`, which reads on its own threads, overlapped with an event, and closes
a handle only after `CancelIoEx` and `GetOverlappedResult` say the read is over. The test also
got a `Watchdog` (`filetree/ops_tests.rs`) that aborts after 30 s and prints the step it was on,
and the workflow got `timeout-minutes: 25`. Both are watchdogs in the §1 sense: they make a hang
fail loudly; they don't make anything pass.

The lesson for this audit: code that waits on another thread or the OS with no event to wait on
fails as a hang, at random, on the slowest machine, long after the change that caused it.

### 2.2 The git slow-mode test

`filetree/git_tests.rs`, `slow_mode_trips_on_two_slow_runs_skips_worktree_edges_and_recovers`:

- the repo counts as slow when a `git status` takes over 250 ms (`slow_run`) and fast again under
  150 ms (`fast_run`), measured by the wall clock;
- a stub git sleeps 0.4 s to be "slow", and the test sleeps 500 ms between runs;
- then it signals two edges, sleeps 400 ms, and asserts that git did not run: a non-event proven
  by sleeping.

Under load a "fast" run can measure slow, and the 400 ms can end before a run that should not
happen does. The neighbouring `signals_during_a_run_collapse_into_one_rerun` sleeps 1200 ms and
then counts runs. Another worker is fixing these on its own branch, by feeding measured durations
in and having the code report the decision it made. Check `git log -- src-tauri/src/filetree/git_tests.rs`
before you start: it is the model for most class C fixes below.

## 3. Inventory

### 3.1 How it was built

Every match below comes from [ast-grep](https://ast-grep.github.io/) run in `src-tauri/src` at
commit 71431aec, excluding `filetree/`. These commands, run as written, give these counts:

```sh
cd src-tauri/src
ast-grep run --lang rust -p '$A::sleep($$$)' --globs '!filetree/**' .
```

| Pattern | Matches |
|---|---|
| `$A::sleep($$$)` | 118 |
| `$A::timeout($$$)` | 41 |
| `$X.recv_timeout($$$)` | 7 |
| `$X.wait_timeout($$$)` | 1 |
| `$A::interval($$$)` | 13 |
| `Instant::now()` | 59 |
| `$A::Instant::now()` | 81 |
| `$X.elapsed()` | 56 |
| `$X.duration_since($$$)` | 63 |
| `SystemTime::now()` | 6 |
| `$A::SystemTime::now()` | 34 |
| `Duration::from_millis($$$)` | 89 |
| `Duration::from_secs($$$)` | 153 |

**ast-grep patterns don't look inside macro bodies.** A call inside `assert!`, `tokio::select!`,
`tokio::join!` or `matches!` is a token tree, not an expression, so `$X.elapsed()` misses
`assert!(t.elapsed() < limit)` and every arm of a `select!`. This rule finds those (41 matches):

```sh
ast-grep scan --globs '!filetree/**' --inline-rules 'id: timing-in-macro
language: rust
rule:
  kind: identifier
  regex: ^(sleep|timeout|interval|recv_timeout|elapsed|duration_since|Instant)$
  inside:
    kind: token_tree
    stopBy: end' .
```

The two sweeps gave 824 matching lines, read one by one. Each row below is one **site**: a wait (a
sleep, a timeout, an interval, a poll loop with a deadline) or a decision (an elapsed time
compared against a threshold). The `Instant::now()` that starts a deadline and the
`Duration::from_*` that is its argument are folded into the site they feed; their values are in
the description. Lines that decide nothing were left out: timestamps stamped into a record, a
duration that is only logged, a `#[ignore]`d benchmark that prints timings.

Columns:

- **Where**: `prod`, or `test` for code under `#[cfg(test)]` (including `#[cfg(all(test, unix))]`).
- **Class**:
  - **A**: a real wait that should become an event (it waits a fixed time hoping something has
    happened, or polls where a notification exists or could).
  - **B**: sleep-then-assert or "give it a moment" in a test. Remove it.
  - **C**: time is the behaviour. Keep it, and inject the clock or durations so tests feed values.
  - **D**: a hang-to-failure net (an HTTP request timeout, a test's deadline whose expiry is a
    failure). Fine as it is.
- **Flag** ⚠: among the most likely to be flaky or to hide a bug.

Line numbers are those at 71431aec. The classes are a proposal: a worker who finds an event the
table missed, or a C that is really an A, should say so in its report and change the row.

### 3.2 Totals

| Module | Sites | prod | test | A | B | C | D | ⚠ |
|---|---|---|---|---|---|---|---|---|
| hive | 91 | 59 | 32 | 17 | 12 | 30 | 32 | 24 |
| mailink | 37 | 37 | 0 | 19 | 0 | 13 | 5 | 15 |
| claude_code | 39 | 29 | 10 | 10 | 8 | 18 | 3 | 17 |
| agent_files | 20 | 7 | 13 | 1 | 11 | 6 | 2 | 11 |
| commands | 35 | 32 | 3 | 7 | 0 | 16 | 12 | 7 |
| mcp_proxy | 28 | 19 | 9 | 3 | 4 | 12 | 9 | 6 |
| pty | 10 | 10 | 0 | 5 | 0 | 5 | 0 | 4 |
| watch | 16 | 9 | 7 | 0 | 5 | 10 | 1 | 8 |
| agent_requests | 1 | 1 | 0 | 0 | 0 | 1 | 0 | 0 |
| browser_link | 3 | 3 | 0 | 1 | 0 | 0 | 2 | 0 |
| comms | 2 | 2 | 0 | 1 | 0 | 1 | 0 | 0 |
| host_guard | 1 | 1 | 0 | 0 | 0 | 1 | 0 | 0 |
| instance_lock | 3 | 1 | 2 | 1 | 1 | 1 | 0 | 1 |
| relocate | 1 | 1 | 0 | 0 | 0 | 0 | 1 | 0 |
| share | 2 | 2 | 0 | 0 | 0 | 0 | 2 | 0 |
| stack | 1 | 1 | 0 | 1 | 0 | 0 | 0 | 1 |
| state | 4 | 4 | 0 | 1 | 0 | 3 | 0 | 0 |
| util | 5 | 3 | 2 | 1 | 0 | 1 | 3 | 0 |
| windows_setup | 1 | 1 | 0 | 0 | 0 | 0 | 1 | 0 |
| **All** | **300** | **222** | **78** | **68** | **41** | **118** | **73** | **94** |

`hive`, `mailink` and `claude_code` hold 46 of the 68 A sites, and the path they share is the
riskiest one: **maiTerm typing into an agent's terminal and then sleeping, hoping the agent has
taken it.** The B sites are spread across the test modules: `hive` 12, `agent_files` 11,
`claude_code` 8, `watch` 5, `mcp_proxy` 4.

### 3.3 Start here: the sites most likely to hide a bug

These are production paths, except the last group. Each line names the site and what goes wrong
when the machine is slow.

**A timeout that reads as success, or as a wrong answer:**

- `claude_code/mod_inbox.rs:283`: once the 3 s wait for the mod's report runs out, the prompt
  counts as delivered (`return true`). A late "dropped" report then loses the message.
- `mailink/mod.rs:4478`, `:4487`: `send_key`/`send_text` press a selector key and sleep 130 or
  280 ms (`NAV_SETTLE_MS`, `ADVANCE_SETTLE_MS`) for the repaint. Navigation is relative, so a slow
  repaint lands on the wrong row and records the wrong answer.
- `claude_code/server.rs:5167`: a hook event counts as a duplicate of the mod's if the same
  session and event name came within 10 s (`MOD_COPY_WINDOW`), so a distinct second event inside
  that window is dropped. It matches by time, not by identity.
- `stack/ports.rs:94`: a port whose listener doesn't accept within 200 ms counts as free.
- `hive/change.rs:393`, `:435`: a DNS lookup that runs out (3 s, 2 s) counts as "doesn't point
  home", the same as a name that doesn't resolve. For the human's own grant and at a worker's
  launch that passes, as documented; a slow resolver and an absent name are told apart by nothing.
- `hive/finish/wake.rs:160`: `Waking::wait` polls for up to 30 s; on expiry `woken_in_terminal`
  drops the wake silently.
- `hive/queue.rs:1272`: a job ends once its exit mark is 5 s old, on the assumption the shell's
  signal was lost; under load the real exit code arrives after that.
- `agent_files/mod.rs:302`: a watcher batch is credited to a shell call when it lands within
  300 ms (`SHELL_TAIL`); a slow watcher drops the label, and a human's save inside the window is
  credited to the agent.
- `pty/manager.rs:1616`, `pty/ssh_watch.rs:73`: a process snapshot's freshness is judged by its
  age (50 ms, 1 s) instead of by whether it was taken after the event it is asked about.

**Typing into an agent on a sleep:**

- `mailink/mod.rs:4171`: `inject_text` pastes, sleeps 120–920 ms, then sends the Enter. It is
  how `submit_prompt` types whenever the mod doesn't take a prompt (phone messages, comms, the
  draft-hold worker, board notices), and how a blocker card's Run types its line. A slow TUI
  takes the Enter as a newline and the message sits unsent.
- `mailink/mod.rs:4252`, `:4259`: 220 ms sleeps around typed image paths.
- `mailink/draft_hold.rs:253` and its caller `claude_code/server.rs:4858`: `await_ready` reads
  the screen every 200 ms for an empty box and 1.2 s of stillness; after 30 s the caller types
  anyway, possibly into a Claude still loading.
- `hive/deliver.rs:107`, `:124`: 1.5 s (`RELEASE_GAP`) after each released delivery, hoping the
  agent took the paste as its own message. `:223` adds 30 s (`AFTER_RESET`) after a usage-limit
  reset, guessing Claude's own wait has resumed.
- `mailink/mod.rs:1719`, `:2020`, `:2171`, `:2349`, `:2445`: fixed waits (45 s, 2 s, 2 s, 3 s,
  2.5 s) for a registration, a dialog closing, a hook, a repaint or a transcript write. `:2349`
  proves a non-event by waiting.
- `hive/events.rs:116`, `:134`: sleeps 2 s after a SessionEnd and 800 ms before reading the
  screen for a dialog; the code's own comment says the permission state can come seconds later.
- `hive/tools.rs:493`: the reviewer is retired 3 s after `hiveVerdict`, hoping it read the reply.
- `hive/retire.rs:387`: 5 s after SIGTERM the tab goes regardless, possibly before the
  transcript's last lines are on disk.

**Waiting a fixed time for another process:**

- `instance_lock.rs:77`: a relaunch waits up to 20 s for the old instance's lock, then hands off
  to a process that is exiting.
- `commands/ssh_tunnel.rs:766`: reclaiming a dropped tunnel's port sleeps 2, 5 and 10 s, hoping
  sshd has reaped the old session; past that the port moves and every agent on the host is
  stranded.
- `commands/accounts.rs:1057`, `:1537`: 5 s without the sign-in shim is taken to mean it isn't
  coming, and the user is shown the wrong link or a false message.
- `pty/ssh_watch.rs:161`, `:170`: sleeps 1200 ms (`START_PROBE_DELAY`, which must stay above an
  800 ms snapshot TTL) and 300 ms (`EXIT_CONFIRM_DELAY`, "long enough for the prompt to finish
  drawing").
- `mcp_proxy/stdio.rs:1009`: sleeps 50 ms "for the stderr reader to count the last lines", and a
  test asserts that count.
- `claude_code/server.rs:127`, `:456`: a dialog counts as closed after two clean screen reads 1 s
  apart; a session counts as dormant after two misses 1.5 s apart.
- `mcp_proxy/mod.rs:627`: a change stream that closes within 5 s is taken to be no stream, and
  that server's changes then wait an hour.
- `watch.rs:634`: a login-PATH probe slower than 5 s caches a fallback PATH for the whole launch.

**Tests that can panic, or pass without testing anything:**

- `Instant::now() - d` panics when the result can't be represented, which on Windows (whose
  `Instant` counts from boot) means a machine up for less than `d`:
  `mailink/accounts.rs:291` (360 s, runs on the Windows CI job) and `commands/ssh_tunnel.rs:1830`
  (31 min, Unix only). Use `checked_sub`, or inject the clock.
- `hive/land/lock.rs:288`: its bound (`RECHECK` + 1 s) is also met by the missed wake it is meant
  to catch, so it can't fail on that bug.
- Non-events proven by sleeping: `watch.rs:862`, `:871`, `watch/remote.rs:301`,
  `agent_files/tests.rs:494`, `:529`, `:541`, `:580`, `mcp_proxy/stdio_tests.rs:636`,
  `hive/queue/tests.rs:843`.
- Wall-clock upper bounds that fail under load: `hive/settings.rs:992`, `commands/editor.rs:1882`,
  `watch.rs:861`, `watch/remote.rs:300`, `agent_files/baseline.rs:738`, `agent_files/tests.rs:484`.
- Ordering by sleep: `claude_code/mod_inbox.rs:491`, `:496`, `claude_code/mod_asks.rs:329`,
  `:345`, `:369`, `:405`, `hive/land/tests.rs:504`, `mcp_proxy/stdio_tests.rs:399`, `:694`.

### 3.4 Every site, by module

#### hive

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `hive/autonomy.rs:574` | prod | The digest timer sleeps 1 h (`HOUR`) between passes that check every Hive for its digest. | C (the hourly tick is the feature. `digest_in` takes `now`, so tests feed it. The timer loop itself is untested and waits real time) |  |
| `hive/change.rs:393` | prod | A DNS lookup of a granted host is capped at 3 s. A timeout counts as "doesn't resolve": the human's grant passes, the queen's is refused. | C (the cap decides the verdict. Inject the duration. Tests use `TEST_PUBLIC` and wait nothing) | ⚠ fails open: a slow resolver skips the private-IP check on the human's path |
| `hive/change.rs:435` | prod | At a worker's launch, `points_home_blocking` waits at most 2 s (`recv_timeout`) for the lookup thread. A timeout means "doesn't point home". | C (the cap decides the verdict) | ⚠ fails open: a slow lookup lets a host through at launch |
| `hive/create.rs:259` | prod | `claude auth status` is capped at 10 s (`output_timeout`). A timeout gives no login warning, which is the documented behaviour of an advisory check. | D |  |
| `hive/create.rs:828` | prod | The ComfyUI detection connects to localhost with a 300 ms timeout. A timeout means "not detected". | D |  |
| `hive/deliver.rs:107` | prod | `in_release_turn` holds the Hive's release turn for 1.5 s (`RELEASE_GAP`) after each released delivery, hoping the agent has taken the paste as its own message. | A (wait on the agent's UserPromptSubmit or turn start) | ⚠ prod sleeps and hopes: under load two pastes can still merge |
| `hive/deliver.rs:124` | prod | `resume_then_release` sleeps 1.5 s (`RELEASE_GAP`) after the resume notice before held deliveries follow. | A (same: wait for the notice to be taken) | ⚠ same hope. Its test (`the_resume_notice_lifts…`, line 446) waits the full 1.5 s of real time |
| `hive/deliver.rs:223` | prod | `wait_out` sleeps until the usage-limit reset plus 30 s (`AFTER_RESET`), then re-checks for a re-armed wait. | C (the reset time is real behaviour. `after` and `limit_wait` are injected, the clock is not). The +30 s "once Claude's own wait has resumed" is an A guess | ⚠ the 30 s margin guesses that Claude resumed: typing into a still-armed wait ends it |
| `hive/deliver.rs:345` | test | Calls `wait_out` with real 10 ms and 20 ms waits and asserts ≥30 ms elapsed. | C (the test of the C site waits real time. Use paused tokio time) |  |
| `hive/deliver.rs:359` | test | Sleeps 50 ms, then asserts the land-hold waiter hasn't finished. | B | ⚠ proves a non-event by sleeping: the waiter may not have run yet, so the test passes without proving the hold |
| `hive/deliver.rs:362` | test | 2 s watchdog on the released waiter. Expiry is a failure. | D |  |
| `hive/deliver.rs:401` | test | Sleeps 50 ms, then asserts the pause-hold waiter hasn't finished. | B | ⚠ proves a non-event by sleeping |
| `hive/deliver.rs:405` | test | 2 s watchdog on the resumed waiter. | D |  |
| `hive/deliver.rs:407` | test | 2 s watchdog on `wait_for_resume` when the Hive isn't paused. | D |  |
| `hive/deliver.rs:419` | test | Each step sleeps 30 ms (gap 5 ms) so that an overlap between released deliveries would be visible. | B (use explicit barriers to force the interleaving) |  |
| `hive/deliver.rs:432` | test | Sleeps 20 ms, then asserts nothing went in while the notice holds the turn. | B | ⚠ proves a non-event by sleeping |
| `hive/deliver.rs:452` | test | Orders the held delivery after the notice with a 10 ms sleep against the notice's 40 ms sleep. | B | ⚠ the order comes from sleep lengths. It also waits the real 1.5 s `RELEASE_GAP` |
| `hive/events.rs:116` | prod | After a worker's SessionEnd, sleeps 2 s (`SETTLE`) assuming the shell has its prompt back, then decides whether it crashed. | A (wait for the tab's OSC 133 prompt mark) | ⚠ prod sleeps and hopes: a slow exit can read as still running or as a crash |
| `hive/events.rs:134` | prod | After a PermissionRequest, Notification or AskUserQuestion hook, sleeps 800 ms (`prompts::SETTLE`), then reads the screen for a dialog to route. | A (wait for the hooks handler's record or a screen change) | ⚠ prod sleeps and hopes: the comment says Claude's permission state comes "seconds" later, longer than 800 ms |
| `hive/feed.rs:114` | prod | The debounce waiter sleeps until the key is due (meta 2 s, diffstat 5 s), reading the monotonic `clock_ms`. | C (the `Debounce` state machine takes `now`, and its tests feed values with no real wait. The spawned loop's clock is not injected) |  |
| `hive/finish.rs:597` | prod | The git ancestry walk from a receipt is capped at 120 s (`WALK_TIMEOUT` → `git::verified_ancestry`). | D |  |
| `hive/finish/wake.rs:160` | prod | `Waking::wait` polls a mutex'd list every 50 ms for up to 30 s (`TURN_WAIT`) for the Hive's turn. | A (`Waking::drop` could notify a `Notify`) | ⚠ on expiry the caller says "try again", but `woken_in_terminal` (line 414) silently drops the wake |
| `hive/finish/wake.rs:407` | prod | A prompt to a Done queen counts as the human's if a human key was pressed in her tab within 30 s (`TYPED_WITHIN_MS`). | A (authorship comes from a time window. maiTerm knows what it typed itself) | ⚠ wrong either way: a peer prompt 29 s after a keypress wakes the Hive, a slow human doesn't. `typed_by_human` is pure and tested with values |
| `hive/fuses.rs:108` | prod | A spawn reservation that isn't confirmed within 120 s (`RESERVATION_TTL`) is pruned, and is refused in `is_live` / `commit` (lines 408, 416). | C (TTL. Every function takes `now`, and tests feed values) |  |
| `hive/gallery/thumbs.rs:35` | prod | Each thumbnailer call (QuickLook, ffmpeg) is capped at 20 s (`STEP_TIMEOUT` → `output_timeout`, lines 397 and 405). | D |  |
| `hive/git.rs:15` | prod | Caps on `git::run` (`util::proc::output_timeout`): `QUERY_TIMEOUT` 15 s, `WORKTREE_TIMEOUT` 600 s. Expiry is an error. | D |  |
| `hive/git.rs:155` | prod | The `cat-file --batch` watchdog thread kills git at the deadline, which reports "timed out". | D |  |
| `hive/land/batch.rs:333` | prod | A background batch land waits on the repo's land lock until `opts.lock_wait` (`LOCK_NOTICE_AFTER`, 1 h). | C (injected through `Opts`) |  |
| `hive/land/driver.rs:241` | prod | `take_locks` waits on each repo's lock until `opts.lock_wait`: 10 s inline (`LOCK_INLINE_WAIT`), then in the background with a notice after 1 h (`LOCK_NOTICE_AFTER`). | C (injected through `Opts`. Tests feed 300 ms and wait it in real time) |  |
| `hive/land/git.rs:20` | prod | Caps on land git: 120 s (`GIT_TIMEOUT`) for merges, checkouts and diffs, 60 s (`PUSH_TIMEOUT`) for pushes. Expiry is an error. | D |  |
| `hive/land/git.rs:141` | prod | Seeding a land worktree's ignored entries is capped at 300 s (`SEED_TIMEOUT`). | D |  |
| `hive/land/git.rs:395` | prod | A busy `index.lock` in Base's checkout is retried 3 times, 2 s apart (`INDEX_LOCK_WAIT`), then reported as held. | C (retry backoff. Not injected) |  |
| `hive/land/lock.rs:107` | prod | An unreadable lock file counts as stale once its mtime is 30 s old (`UNREADABLE_STALE`). | C (crash-recovery TTL. Wall clock, not injected) |  |
| `hive/land/lock.rs:171` | prod | A waiting land wakes on this process's releases (condvar), but re-reads another process's lock file every 2 s (`RECHECK`). | A (cross-process: a blocking flock or a watcher on the lock file would be the event) |  |
| `hive/land/lock.rs:255` | test | `take` against pid 1's lock with a 300 ms deadline, asserting Busy and ≥300 ms elapsed. | C (the test waits real time) |  |
| `hive/land/lock.rs:288` | test | Sleeps 150 ms so the waiter parks, releases, then asserts elapsed < `RECHECK` + 1 s. | B | ⚠ can't fail on the bug it names: a missed wake re-checks at about 2 s, still under 3 s |
| `hive/land/tests.rs:126` | test | The default `lock_wait` of 20 s in test `Opts`: a lock wait that should never expire. | D |  |
| `hive/land/tests.rs:504` | test | The first land's `before_advance` sleeps 400 ms while the second starts 100 ms later (line 506, missing from the input). The test expects b to land second. | B (gate the second land on a hook signal) | ⚠ the order comes from sleeps on a 4-thread runtime: a slow start of a flips `merged_base` |
| `hive/land/tests.rs:533` | test | Busy-lock outcome with a 300 ms lock wait, waited in real time. | C |  |
| `hive/land/tests.rs:723` | test | A 200 ms sleep in `before_advance` makes two opposite-order lands overlap. | B (force the overlap with a barrier) |  |
| `hive/land/tests.rs:724` | test | 60 s deadlock watchdog. Expiry is a failure. | D |  |
| `hive/land/tests.rs:2204` | test | `STATE_LOCKING` with a 300 ms lock wait, waited in real time. | C |  |
| `hive/land/tests.rs:2241` | test | The fake checker reports its queued batch check after a 100 ms sleep. | B (whether the awaited path runs depends on timing: an early end goes through `Slot::Ended`) |  |
| `hive/land/undo.rs:415` | prod | An undo waits on the repo's land lock until `opts.lock_wait`. | C (injected through `Opts`) |  |
| `hive/launch.rs:647` | prod | The `claude --version` probe is capped at 15 s. A timeout means "version unknown". | D |  |
| `hive/media/whisper.rs:284` | prod | The Whisper model download client: 20 s connect timeout, 60 s read timeout. | D |  |
| `hive/media/whisper.rs:313` | prod | The HEAD request for the model's facts: 20 s connect timeout. | D |  |
| `hive/media/whisper.rs:406` | prod | Download progress is emitted every 1%, or every 500 ms, or at the end. | C (rate limit. Not injected) |  |
| `hive/memory/mod.rs:430` | prod | The memory folder watcher uses `notify_debouncer_mini` with a 1.5 s debounce (`DEBOUNCE_MS`). | C (the library's clock can't be injected. Its test waits real time) |  |
| `hive/memory/tests.rs:386` | test | Sleeps 300 ms after `start_watcher`, hoping FSEvents is armed before the write. | B | ⚠ depends on load: a write before the watcher arms is never seen |
| `hive/memory/tests.rs:389` | test | Polls MEMORY.md every 100 ms for up to 3 s for the watcher's rebuild (1.5 s debounce plus FSEvents latency). | A (the `on_change` callback could signal a channel) | ⚠ depends on load: a 1.5 s debounce inside a 3 s budget |
| `hive/new_goal.rs:630` | prod | A follow-up added to a queen during a New goal is let through as a put-back if a delivery took it within the last 10 min (`TAKEN_FOR`). | A (the take could hand its put-back a token instead of a time window) |  |
| `hive/owed.rs:123` | prod | Typed-send records are kept for 1 h (`TYPED_KEEP_MS`, "as long as maiLink keeps an outcome"). | C (retention TTL. Wall clock, not injected) |  |
| `hive/owed.rs:310` | prod | A wake relaunch holds the human's further messages for 180 s (`WAKE_RELAUNCH_FOR`) unless its session starts first (`end_wake_relaunch`). | C (TTL on a missing session start. Not injected) |  |
| `hive/pressure.rs:107` | prod | `recent` reuses a memory/CPU sample that is less than 5 s old (`SAMPLE_EVERY`). | C (cache TTL. Instant not injected) |  |
| `hive/pressure.rs:149` | prod | `until_open` re-reads the spawn gate every `every` (5 s from `waiters.rs:101`) until memory pressure eases. | C (periodic sampling. The interval is injected and the test feeds 1 ms. macOS's memory-pressure dispatch source could make it A) |  |
| `hive/probe.rs:162` | test | A real `claude -p` run is capped at 300 s (the same at line 680). `expect` fails on expiry. | D |  |
| `hive/probe.rs:328` | test | The fake maiTerm's non-blocking accept loop sleeps 10 ms between polls. | A (blocking accept plus a wake connection, as `serve.rs` `DeadProxy` does) |  |
| `hive/probe.rs:351` | test | 10 s read timeout on a fake-maiTerm connection. | D |  |
| `hive/probe.rs:533` | test | Sleeps 3 s "past the watcher's debounce", then rebuilds and asserts on the memory index. | B | ⚠ sleep then assert: a debounced rebuild still in flight can race the explicit one |
| `hive/queue.rs:1272` | prod | The ticker ends a job from its exit mark once the mark is 5 s old (`LOST_EXIT_AFTER`), on the assumption that its D was lost. | A (it proves a non-event, the missing D, by age) | ⚠ under load the ticker can end a job from its marks before a slow D arrives, so the exit code is lost |
| `hive/queue.rs:1337` | prod | The queue ticker runs every 5 s (`SAMPLE_EVERY`): job timeouts, idle serves, lost exits, memory pressure, then schedule. It stops when the queue is idle. | C (periodic tick. The queue logic takes `now` and tests feed `T0`. The tick itself waits real time) |  |
| `hive/queue/tests.rs:213` | test | 10 s watchdog that `end_lost_exits` returns. Expiry is a failure. | D |  |
| `hive/queue/tests.rs:843` | test | Types the sandboxed TIOCSTI injector, sleeps 3 s, then types `exit` and checks whether the escape file exists. | B (wait for the injector's own completion mark) | ⚠ the negative case proves a non-event by sleeping, and a slow python makes the control flaky |
| `hive/quiet.rs:176` | prod | A worker quiet for 10 min (`QUIET`) after a Stop gets a notice to its coordinator, unless a newer event bumped the generation. A paused Hive re-arms it. | C (slow detection. Not injected. Tests check generations and wait nothing) |  |
| `hive/registry_proxy.rs:139` | prod | The client has 10 s to send its CONNECT head (`HEAD_TIMEOUT`), otherwise the connection is dropped. | D |  |
| `hive/registry_proxy.rs:164` | prod | The upstream connect is capped at 10 s (`CONNECT_TIMEOUT`), otherwise 502. | D |  |
| `hive/registry_proxy.rs:186` | test | 10 s read timeout on the test client. | D |  |
| `hive/retire.rs:387` | prod | After SIGTERM, polls the tab's foreground every 250 ms for up to 5 s (`EXIT_GRACE`) for the shell prompt, then goes on to drop the tab. | A (wait for the PTY child's exit or the OSC 133 prompt) | ⚠ expiry falls through silently: the tab goes, possibly before the transcript's last lines are on disk |
| `hive/rpc.rs:51` | prod | A webview job-start request is capped at 90 s (`JOB_START_TIMEOUT`, used at `queue.rs:1766` and `media/install.rs:490`). Expiry fails the start. | D (sized to outlast the frontend stack guard's own waits) |  |
| `hive/rpc.rs:209` | prod | A webview RPC is capped at 20 s (`TIMEOUT`), or 45 s for a relaunch (`RELAUNCH_TIMEOUT`, sized to outlast the frontend's 20 s quiet wait). | D |  |
| `hive/rpc.rs:354` | prod | Sleeps until a routed prompt's due time, then calls `prompts::on_due` with `now`. | C (the stub window in tests skips the sleep) |  |
| `hive/run.rs:672` | prod | A finished run's result delivery sleeps until enqueue + 5 s + 750 ms, so that the inline poller has had its chance to take it. | A (the hand-off between the two paths is by clock. The inline waiter could mark itself done) |  |
| `hive/run.rs:842` | prod | `hiveRun` polls the queue every 250 ms for up to 5 s (`INLINE_WAIT`) for a short run's result. | A (poll: queue completion could notify. The 5 s inline window itself is C) |  |
| `hive/serve.rs:159` | prod | reqwest client timeout: 20 s for `hiveFetch` (`FETCH_TIMEOUT`, line 267). | D |  |
| `hive/serve.rs:192` | prod | `wait_http` probes the served port every 400 ms (3 s per probe) until anything answers HTTP, the job stops, or the deadline passes. | C (readiness of an arbitrary app has no event. Its tests wait real time) |  |
| `hive/serve.rs:423` | prod | `DeadProxy::drop` makes a 200 ms-capped localhost connect to wake its accept loop. | D |  |
| `hive/serve.rs:435` | prod | The screenshot browser is killed after 45 s (`SHOT_TIMEOUT`), which is an error. | D |  |
| `hive/serve.rs:797` | prod | A serve polls the queue every 300 ms for RUNNING plus a port until `readyTimeout` (default 120 s, at most 600 s), then gives the rest (at least 5 s) to `wait_http`. | A (poll: a queue state change could notify) |  |
| `hive/serve.rs:871` | prod | `ended_answer` polls for the run's result up to 10 times, 300 ms apart, because it is "recorded just after it leaves the queue". After that it falls back to a note. | A (wait on the result record) | ⚠ prod polls and hopes: under load the result misses the 3 s and arrives as a message instead |
| `hive/serve/tests.rs:103` | test | The fake server binds after a 900 ms sleep, and the test asserts `wait_http` took ≥800 ms. | C (the C site's test waits real time. Bind on a signal instead) |  |
| `hive/serve/tests.rs:108` | test | 10 s deadline on `wait_http` that should never expire. | D |  |
| `hive/serve/tests.rs:114` | test | A dead port is waited out for 1.2 s of real time to see the timeout error. | C |  |
| `hive/serve/tests.rs:117` | test | 30 s deadline that never runs, because `alive` errs first. | D |  |
| `hive/settings.rs:495` | prod | `ssh -G` is polled with `try_wait` every 20 ms and killed after 3 s (`SSH_G_TIMEOUT`), which is an Err read as "could be anywhere" (fails closed). Its test feeds 300 ms (line 990). | D |  |
| `hive/settings.rs:992` | test | `ssh -G` stub that sleeps 30 s, killed at a 300 ms timeout; asserts `elapsed < 5 s` | C | ⚠ upper wall-clock bound |
| `hive/tool_setup.rs:376` | prod | An approved capability watch that nobody finished expires after 24 h (`WATCH_TTL_MS`). | C (TTL. Wall clock, not injected) |  |
| `hive/tools.rs:493` | prod | 3 s after `hiveVerdict` (`review::RETIRE_AFTER`), retires the reviewer, hoping the tool's answer reached it. | A (wait for the reviewer's PostToolUse or Stop hook) | ⚠ prod sleeps and hopes: a slow agent is killed before it reads the reply |
| `hive/trust.rs:23` | prod | The trust-key git call is capped at 15 s (`GIT_TIMEOUT`, line 62). | D |  |
| `hive/waiters.rs:66` | prod | Sleeps until the usage-limit breaker ends plus 1 s, then polls every Hive's waiters. | C (`next_poll` is pure and tested. The sleep reads the wall clock) |  |
| `hive/worktree.rs:536` | prod | The macOS `cp -c` seed of a worktree is capped at 600 s (`CP_TIMEOUT`, line 540). | D |  |

#### mailink

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `mailink/accounts.rs:211` | prod | Suppresses the `account` doorbell ring for a remote handoff failure seen within 180 s (`RING_BASELINE_WINDOW`) of app start; clock half-injected via `app.started_at`, and tests back-date it rather than wait | C | ⚠ the test fixture (line 291) does `Instant::now() - 360 s`, which panics on a runner booted less than 6 min ago (fresh CI VM) |
| `mailink/draft_hold.rs:253` | prod | `await_ready` polls every 200 ms until a starting Claude's box reads empty and its screen has been still for 1200 ms (`READY_QUIET_MS`), up to `within` (30 s from claude_code/server.rs:4858); on expiry the caller delivers anyway | A | ⚠ polls the screen with no output event; on a slow start the 30 s expiry types into a Claude that is still loading (the CR-as-newline bug it exists to stop) |
| `mailink/draft_hold.rs:424` | prod | Held-message worker wakes every 1 s (`POLL_MS`) to re-check draft/typing/dialog/limit hold reasons and expires items older than 30 min (`HOLD_MAX_MS`); no event wakes it when a hold clears | A |  |
| `mailink/follow_ups.rs:38` | prod | A watch-script follow-up whose `expires_at` is before `now_ms` no longer awaits approval; `now` is already a parameter, so no real waits | C |  |
| `mailink/inline_image.rs:443` | prod | Remote image read over ssh capped at 30 s (`kill_on_drop`), expiry is a clear "took too long" error | D |  |
| `mailink/mirror.rs:102` | prod | SSH transcript mirror skips fetches for 30 s (`FAILURE_BACKOFF_MS`) after a failed one; wall clock read directly, not injected, untested | C |  |
| `mailink/mirror.rs:208` | prod | One ssh fetch of the remote transcript delta capped at 20 s; expiry returns false and the caller applies the backoff | D |  |
| `mailink/mirror.rs:401` | prod | Startup sweep deletes shadow transcripts not modified for 30 days (`PRUNE_AFTER_SECS`); `SystemTime::now()` not injected | C |  |
| `mailink/mod.rs:317` | prod | maiLink disable gives axum 1 s of graceful shutdown before connections are force-closed; untested | C |  |
| `mailink/mod.rs:1719` | prod | Wake polls `tab_registered` every 400 ms for up to 45 s (`WAKE_BUDGET_MS`) after emitting the wake, when SessionStart registration is an event it could await; on expiry delivers anyway if the agent process is up | A | ⚠ expiry falls back to delivering into an agent that never registered |
| `mailink/mod.rs:2020` | prod | After typing the trust-dialog keys, polls the screen every 100 ms ×20 (2 s) for the dialog to close, else reports `inject_failed` | A | ⚠ a loaded machine reports a failure for keys that did land |
| `mailink/mod.rs:2171` | prod | After driving AskUserQuestion keys, polls every 100 ms ×20 (2 s) for the PostToolUse hook to clear `pending_question`, else `inject_failed` | A | ⚠ hook arriving after 2 s gives a false failure, and the selector_dirty guard then refuses a retry |
| `mailink/mod.rs:2349` | prod | After a phone interrupt, polls PTY output every 50 ms for up to 3 s (`COMPOSER_SETTLE_CAP_MS`) for the ESC repaint; no output in 3 s is taken as "nothing was cancelled" and the composer is left alone | A | ⚠ proves a non-event by waiting: a repaint after 3 s leaves restored text that merges into the next phone message |
| `mailink/mod.rs:2360` | prod | Then waits (50 ms polls, same 3 s cap) for PTY output to be quiet ≥250 ms (`COMPOSER_QUIET_MS`) before sending Ctrl+L; at the cap it clears anyway | C | ⚠ cap expiry sends the clear mid-redraw, the case the wait exists to avoid; quiet window not injectable |
| `mailink/mod.rs:2445` | prod | Queue cancel polls the transcript every 150 ms for up to 2.5 s (`QUEUE_RECALL_WAIT_MS`) for the queue to empty after cursor-up, else answers `not-recalled` | A | ⚠ a late transcript write leaves the recalled message sitting uncleared in the composer while the phone is told nothing happened |
| `mailink/mod.rs:2536` | prod | New conversation polls every 100 ms ×50 (5 s, `NEW_TAB_WAIT_TICKS`) for the frontend-minted tab to appear and be designated, else `timeout` (tab may still appear) | A |  |
| `mailink/mod.rs:2610` | prod | `kill_pid` SIGTERMs a background shell, polls liveness every 100 ms ×10, SIGKILLs after 1 s; untested | C |  |
| `mailink/mod.rs:2616` | prod | After SIGKILL sleeps 100 ms, then reports whether the pid is gone as `stopped` | A | ⚠ sleep-then-check: a slow reap reports `stopped:false` for a killed shell |
| `mailink/mod.rs:2814` | prod | Phone request approval polls `agent_requests` every 200 ms for up to 15 s (`REQUEST_SETTLE_WAIT`) for the status to leave `approved`; expiry honestly says "still carrying it out" | A |  |
| `mailink/mod.rs:3075` | prod | Pairing code is valid only before its expiry `Instant`, minted 120 s ahead at line 6893; `Instant::now()` direct, expiry untested | C |  |
| `mailink/mod.rs:3391` | prod | WS loop rebuilds and diffs the chat snapshot every 1.5 s to push `chat_state`/`attention`/`chats_changed`; the hook state machine could push these | A |  |
| `mailink/mod.rs:3394` | prod | WS loop checks transcript mtimes every 400 ms (Delay) to stream new turns; a file watcher or hook event could drive it | A |  |
| `mailink/mod.rs:3403` | prod | WS loop schedules an SSH mirror fetch every 5 s while a phone watches, because a long remote turn appends with no hook | A |  |
| `mailink/mod.rs:3420` | prod | WS heartbeat: pings every 20 s (`WS_PING_INTERVAL`), closes if no pong within 20 s of the ping (decision at 3451); also logs a loop stall when a tick is >25 s late. Not injectable, untested | C |  |
| `mailink/mod.rs:4171` | prod | `inject_text` sleeps 120–920 ms (scaled by text length) after a bracketed paste, then sends the CR | A | ⚠ sleep-and-hope on the main typing path: a slow TUI takes the CR as a newline and the message sits unsent |
| `mailink/mod.rs:4252` | prod | Sleeps 220 ms (`IMAGE_SETTLE_MS`) between typed image paths so each becomes an `[Image #N]` chip | A | ⚠ a slow TUI misses the chip conversion |
| `mailink/mod.rs:4259` | prod | Sleeps 220 ms after the last image path before the caption and CR | A | ⚠ same; CR can land before the last chip attaches |
| `mailink/mod.rs:4415` | prod | Push of image bytes to an ssh host capped at 30 s, expiry is an error (the child lacks `kill_on_drop`, so a timed-out ssh lives on) | D |  |
| `mailink/mod.rs:4448` | prod | Remote `cat` read over ssh capped at 30 s, expiry is an error (no `kill_on_drop` either) | D |  |
| `mailink/mod.rs:4478` | prod | `send_key` sleeps 130 ms (`NAV_SETTLE_MS`) or 280 ms (`ADVANCE_SETTLE_MS`) after each selector keystroke, hoping it repainted | A | ⚠ navigation is relative: a lagging repaint lands on the wrong row and records the wrong answer |
| `mailink/mod.rs:4487` | prod | `send_text` sleeps 280 ms after typing into the selector's Other field | A | ⚠ same selector race |
| `mailink/mod.rs:4894` | prod | `send_keys` waits 40 ms between named keys so a lone ESC plus the next key isn't read as Alt+key | C | ⚠ a slow reader still gets both bytes in one read |
| `mailink/mod.rs:6950` | prod | Doorbell loop wakes every 2 s to diff attention state and ring uncovered phones; inner windows (3 s WS grace, 30 s ring collapse) already take `now_ms` as an argument and `ws_covered` is tested with fed values | A |  |
| `mailink/rpc.rs:87` | prod | Webview RPC awaits its oneshot ack for up to 15 s (`TIMEOUT`, or the caller's), expiry is an explicit `Outcome::Timeout` | D |  |
| `mailink/subagents.rs:514` | prod | A running subagent is settled as ended when its sidecar is >2 h old (`SIDECAR_STALE_MS`), or with no sidecar >12 h after launch (line 511); `now` injected, tests pure | C |  |
| `mailink/transcript.rs:611` | prod | `memo_by_transcript` serves a stale derived value for up to 5 s (`MEMO_REPARSE_MIN`) after the transcript changes (rate-limits reparsing); `Instant` direct, untested | C |  |
| `mailink/transcript.rs:1493` | prod | A failed transcript lookup is remembered for 5 s (`LOCATE_MISS_TTL`); not injectable, and its test only passes if it runs within the real 5 s window | C |  |

#### claude_code

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `claude_code/claude_mod.rs:124` | test | Sleeps 20 ms between two `write_files` calls so a rewrite would change the file's mtime, then asserts the mtime is unchanged (proves "not rewritten" by clock) | B | ⚠ non-event proven by sleeping; on a filesystem with 1 s / 2 s mtime granularity a rewrite inside the same tick passes vacuously |
| `claude_code/lockfile.rs:85` | prod | Probes a tunnel lockfile's port with a 250 ms `connect_timeout`; expiry reads as "not listening", and the caller deletes the lockfile and sweeps its hooks | C |  |
| `claude_code/mod_asks.rs:63` | prod | `is_live`: an ask with no parked poll and no poll for 5 s (`STALE_AFTER`) counts as gone (hook abandoned) | C |  |
| `claude_code/mod_asks.rs:69` | prod | `is_kept`: a taken ask stays in the map 30 s after it was taken, waiting for `deliver` to report it, then is pruned | C |  |
| `claude_code/mod_asks.rs:147` | prod | Long-poll round: parks on the ask's `Notify` for at most `min(wait, MAX_WAIT=10 s)` (must end before the mod's ~30 s fetch dies); woken early by the answer | C |  |
| `claude_code/mod_asks.rs:240` | prod | `deliver`: after handing over the answer, re-checks every 25 ms whether the hook took it, retracting it after `DELIVERY_TIMEOUT` 2 s (the take could notify instead of being polled); its test waits the real 2 s | A | ⚠ a hook round-trip slower than 2 s on a loaded machine is retracted and the answer falls back to keystrokes |
| `claude_code/mod_asks.rs:329` | test | Spawns a 5 s poll, sleeps 50 ms hoping it has parked, then asserts `polling == 1` | B | ⚠ fails if the spawned task hasn't run within 50 ms |
| `claude_code/mod_asks.rs:345` | test | Spawns `deliver`, sleeps 50 ms hoping the answer is set, then asserts the poll returns within 1 s of real time | B | ⚠ if `deliver` hasn't run yet the poll parks and is woken anyway, so the "before the poll parks" case is never proven; the `elapsed() < 1 s` assert is load-dependent |
| `claude_code/mod_asks.rs:357` | test | 100 ms watchdog on `notified()` with a stored permit; expiry is a test failure | D |  |
| `claude_code/mod_asks.rs:369` | test | Spawns `deliver`, sleeps 50 ms hoping it is waiting, then settles the ask and asserts not taken | B | ⚠ if `settle_call` runs first `deliver` finds no ask and returns false anyway: passes without testing the race |
| `claude_code/mod_asks.rs:405` | test | Spawns a 5 s poll, sleeps 50 ms hoping it has parked, then delivers | B | ⚠ if not parked the poll takes the answer at once: passes without testing "reaches a parked poll" |
| `claude_code/mod_inbox.rs:99` | prod | `Inbox::is_live`: no parked poll and none for 5 s (`STALE_AFTER`) means no mod is listening (tests back-date `last_poll`, no real wait) | C |  |
| `claude_code/mod_inbox.rs:145` | prod | `poll` forgets tabs nobody has polled for 600 s (`FORGET_AFTER`) | C |  |
| `claude_code/mod_inbox.rs:164` | prod | Long-poll round: parks on the tab's `Notify` for at most `min(wait, MAX_WAIT=10 s)`; woken by an offer (tests pass 1 ms waits) | C |  |
| `claude_code/mod_inbox.rs:222` | prod | `has_unstarted` drops acked prompts with no `started` report after 6 h (`UNSTARTED_FOR`) | C |  |
| `claude_code/mod_inbox.rs:255` | prod | `deliver`: re-checks every 20 ms whether the mod acked the offer, retracting it after `DELIVERY_TIMEOUT` 2 s (the ack could notify); its test waits the real 2 s and asserts `elapsed >= 2 s` | A | ⚠ an ack slower than 2 s under load retracts the offer and the caller types the prompt instead |
| `claude_code/mod_inbox.rs:283` | prod | Between turns, re-checks every 20 ms for the mod's `started` report for up to `START_WAIT` 3 s; on expiry returns `true` (counted as delivered) | A | ⚠ expiry is a silent pass: a late report of a DROPPED prompt (the Esc bug this guards) after 3 s reads as delivered and the message vanishes |
| `claude_code/mod_inbox.rs:353` | test | `parked_poll`: spawns a 5 s poll and sleeps 30 ms hoping it has parked (used by most tests in the file) | B | ⚠ if the poll hasn't registered the inbox, `deliver` finds no live mod and returns false: load-dependent failure |
| `claude_code/mod_inbox.rs:479` | test | asserts the real `DELIVERY_TIMEOUT` (2 s) elapsed before a lost ack was retracted, so the test waits it out | C |  |
| `claude_code/mod_inbox.rs:491` | test | Sleeps 10 ms after spawning the first `deliver` so it queues before the second | B | ⚠ ordering by sleep: if the second spawn runs first, "oldest first" fails |
| `claude_code/mod_inbox.rs:496` | test | Sleeps 10 ms hoping the second `deliver` has queued before a 1 ms poll | B | ⚠ if not queued the 1 ms poll returns Empty and the test panics |
| `claude_code/server.rs:127` | prod | `watch_unattributed_prompt` reads the screen once a second; two consecutive "no dialog" reads release a WaitingPermission state | A | ⚠ production poll-and-hope: "closed" is defined as two reads 1 s apart; could re-read on the tab's render/output event instead |
| `claude_code/server.rs:426` | prod | Every 30 s (`MCP_REASSERT_INTERVAL`) re-asserts maiTerm's MCP entries in `~/.claude.json` and proxy entries in case the `claude` CLI clobbered them (a file watch would see the drift) | A |  |
| `claude_code/server.rs:456` | prod | Dormancy reaper polls non-Claude agent tabs' process trees every 1.5 s and synthesizes SessionEnd after 2 consecutive "agent gone" reads | A | ⚠ an auto-resume exit→relaunch gap or sysinfo lag longer than ~3 s ends a live session; the PTY child's exit could be the event |
| `claude_code/server.rs:752` | prod | Follow-up priming treats a follow-up whose `expires_at` is before `SystemTime::now()` as expired; clock not injected, no test of it | C |  |
| `claude_code/server.rs:2883` | prod | IDE WebSocket sends a ping every 30 s (`PING_INTERVAL`), missed ticks skipped | C |  |
| `claude_code/server.rs:3025` | prod | MCP proxy SSE stream: 15 s (`SSE_KEEPALIVE_INTERVAL`) with no message emits a keepalive comment | C |  |
| `claude_code/server.rs:3125` | prod | Legacy `/sse` stream: 15 s with no message emits a keepalive comment | C |  |
| `claude_code/server.rs:3144` | prod | Cleanup task sleeps 15 s at a time, checking whether the SSE session's sender is closed, then tears down the session (`UnboundedSender::closed().await` would be the event) | A |  |
| `claude_code/server.rs:3903` | prod | initSession prunes the pending SessionStart pool of entries older than 30 s before claiming one | C | ⚠ the session-to-tab claim is matched by time: an initSession later than 30 s after its SessionStart claims nothing (no session id, no auto-resume) |
| `claude_code/server.rs:4379` | prod | Waits up to 120 s (`RESPONSE_TIMEOUT`) for the frontend to answer an IDE tool request; expiry answers an error | D |  |
| `claude_code/server.rs:4775` | prod | Hook gives a Write's `symlink_metadata` 25 ms (`WRITE_EXISTS_BUDGET`, including `spawn_blocking` scheduling); expiry records "unknown" create-vs-overwrite | C |  |
| `claude_code/server.rs:4858` | prod | Hive session start waits up to 30 s for `await_ready` (mailink: empty input box + screen quiet 1.2 s, polled every 200 ms), then delivers anyway with a warning | A | ⚠ readiness is guessed from screen stillness; a slow start past 30 s types into a Claude still loading |
| `claude_code/server.rs:4952` | prod | `deliver_hive_owed` retries a refused delivery after 0, 2, 5, 10, 20 s, hoping the process snapshot has caught up with `claude` owning the terminal | A | ⚠ retry-and-hope on a stale snapshot; past ~37 s the kickoff/digest waits for the next session start |
| `claude_code/server.rs:4976` | prod | Waits up to 31 min for a held message's outcome event, a net past the hold's own 30 min expiry (which reports "dropped") | D |  |
| `claude_code/server.rs:5068` | prod | PreCompact asks the window over RPC with a 3 s timeout (`IDLE_COMPACTION_ANSWER`), passing the window a 2.5 s deadline; no answer lets Claude compact normally | C |  |
| `claude_code/server.rs:5167` | prod | Drops a settings-hook copy of an event when the mod sent the same session+event name within 10 s (`MOD_COPY_WINDOW`); the record is pruned after 60 s | A | ⚠ duplicates matched by time and event name, not identity: a mod that stops sending loses up to 10 s of hook events, and a distinct second event of the same name inside 10 s is dropped |
| `claude_code/server.rs:5352` | prod | Contested-session records older than 24 h are pruned when a new rebind is recorded | C |  |
| `claude_code/server.rs:5403` | prod | SessionStart with no tab prunes pending-pool entries older than 30 s before buffering this one (same pool and rule as :3903) | C |  |

#### agent_files

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `agent_files/baseline.rs:354` | prod | A hook waits at most 25 ms (`DEADLINE`) for turn-baseline reads, including `spawn_blocking` scheduling; on expiry they are cancelled and recorded `timeout`. Deadline injected via `Baselines::with`; most tests use `ROOMY` 5 s | C |  |
| `agent_files/baseline.rs:645` | prod | The Turn lens's line diff stops looking for the shortest edit after 200 ms (`COUNT_DEADLINE`, a compute budget); injectable via `line_counts_within` | C |  |
| `agent_files/baseline.rs:730` | test | The fake slow reader sleeps 200 ms so it outlives the 25 ms deadline (could block on a gate the test releases) | B |  |
| `agent_files/baseline.rs:738` | test | Asserts `capture` returned within 100 ms of real time against the 25 ms deadline | B | ⚠ load-dependent: a stalled runner exceeds 100 ms even though the deadline held |
| `agent_files/baseline.rs:743` | test | Sleeps 350 ms to "let the read finish", then asserts the late read was cancelled and discarded | B | ⚠ if the 200 ms fake read hasn't finished within 350 ms, `saw_cancel` is still false and the test fails |
| `agent_files/baseline.rs:1030` | test | Asserts a 50 ms-limited diff of 40k changed lines took under 2 s (40x headroom; expiry is a failure) | D |  |
| `agent_files/mod.rs:297` | prod | A shell call with no end seen counts as running for at most 600 s (`SHELL_MAX`), then loses attribution; clock not injected, untested | C |  |
| `agent_files/mod.rs:302` | prod | A shell call ended within 300 ms (`SHELL_TAIL`) still owns watcher-reported changes (sized to cover the watcher's 250 ms batch close) | A | ⚠ ties a watcher batch to a shell call by time: watcher latency over 300 ms under load drops the label, a human save inside the window gets credited; a batch sequence/flush marker is the event |
| `agent_files/mod.rs:673` | prod | Ended shell calls are pruned once older than 1.2 s (`SHELL_TAIL * 4`), on every hook and in `sole_shell_in` | C |  |
| `agent_files/mod.rs:942` | prod | `prune` forgets a tab missing from every window for 60 s (`MISSING_GRACE`), so a reload's remap can move the ledger first; tests use the `age_missing` helper (no real wait) | C |  |
| `agent_files/mod.rs:1054` | prod | Dispatcher blocks with no timeout when idle, else `recv_timeout` until the next coalesced batch is due; at most one batch per tree per 100 ms (`COALESCE`). `take_due(now)` takes the clock (tests feed `t0 + 50 ms`), `next_due` still reads `Instant::now()` | C |  |
| `agent_files/tests.rs:345` | test | Sleeps 5 ms so a later idle event would stamp a different `end_ms`, then asserts the end didn't move | B | ⚠ non-event proven by the clock; with a coarse clock it passes vacuously |
| `agent_files/tests.rs:440` | test | `wait_for` helper: polls a condition every 10 ms until a deadline (30 s watcher, 10 s listing/status/commit, 5 s attribution, 2 s marks); expiry is a failure | D |  |
| `agent_files/tests.rs:453` | test | `open_tree` sleeps 400 ms after the watcher starts so FSEvents' pre-start reports "settle" | B | ⚠ a late stray FSEvents batch after 400 ms on a loaded machine gets attributed in later checks; a sentinel write awaited as an event would bound it |
| `agent_files/tests.rs:484` | test | Asserts the first agent mark arrived within 500 ms of real time after `record` | B | ⚠ load-dependent timing assertion |
| `agent_files/tests.rs:494` | test | Sleeps 250 ms, then asserts no mark arrived for a path outside the root | B | ⚠ non-event proven by sleeping |
| `agent_files/tests.rs:529` | test | Sleeps 500 ms after the attribution, then asserts the ignored `build/` path was never attributed | B | ⚠ non-event proven by sleeping |
| `agent_files/tests.rs:536` | test | Sleeps 500 ms after the Post so the real 300 ms `SHELL_TAIL` runs out (and stray batches drain) before the two-shell phase | B | ⚠ waits out a production timer on the real clock; a slow watcher batch from phase 1 can still land in phase 2 |
| `agent_files/tests.rs:541` | test | Sleeps 1200 ms, then asserts `both.txt` was credited to neither shell | B | ⚠ non-event proven by sleeping: a watcher slower than 1.2 s makes it pass without testing anything |
| `agent_files/tests.rs:580` | test | Sleeps 1500 ms, then asserts the commit made with two turns open was not credited | B | ⚠ non-event proven by sleeping |

#### commands

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `commands/accounts.rs:1057` | prod | Sign-in: if the `open` shim has not captured the real URL 5 s (SHIM_GRACE) after start, the printed paste-code URL from the transcript is announced instead, and `announced` latches | A — the runtime printing its URL is observable; decide on "printed but no shim" by event, not on 5 s | ⚠ non-event proven by waiting: on a loaded machine a shim later than 5 s loses to the dead-end paste-code link for good |
| `commands/accounts.rs:1142` | prod | Sign-in kills the CLI once `start.elapsed()` passes the deadline (caller's, default 300 s, clamped 30–900 s) | C — kill-after-timeout; deadline is a parameter but the clock is read inline; no test waits |  |
| `commands/accounts.rs:1148` | prod | Sign-in loop sleeps 250 ms between checks of shim capture, child exit and cancellation | A — child exit (waiter thread), cancel and shim capture can each signal a channel |  |
| `commands/accounts.rs:1537` | prod | Token mint: 5 s (SHIM_GRACE) with no shim capture emits "the agent opened its own window — approve there" (not latched; a later real URL is still announced) | A — same as 1057 | ⚠ non-event proven by waiting: a loaded machine shows the user a false message |
| `commands/accounts.rs:1593` | prod | Token mint gives up once elapsed passes its deadline (default 600 s, clamped 60–1800 s) | C — kill-after-timeout; clock inline; no test waits |  |
| `commands/accounts.rs:1600` | prod | Mint loop sleeps 200 ms between checks of cancel, a token in the PTY transcript, and child exit | A — the PTY reader thread could signal on each chunk; child exit and cancel likewise |  |
| `commands/accounts.rs:2064` | prod | A remote token is treated as expired when now > minted_at + TOKEN_LIFETIME_SECS (365 d) and is not sent | C — TTL; `SystemTime::now()` read inline, not injectable; no test waits |  |
| `commands/accounts.rs:2296` | prod | Remote token handoff waits ≤ 15 s for the ssh child to finish, else "host did not answer within 15s" | D — note: the tokio child is not `kill_on_drop`, so a timeout leaves the ssh running |  |
| `commands/editor.rs:152` | prod | `identities_within` waits on each path's condvar until IDENTITY_DEADLINE (1000 ms); a path not stat'd in time is unknown and the open falls back to comparing spellings | C — slow detection of a dead mount; the duration is already a parameter |  |
| `commands/editor.rs:220` | prod | Remote path resolve polls the ssh child every 25 ms and kills it after 5 s (REMOTE_RESOLVE_DEADLINE) → "host did not answer in time" | D — the hand-rolled poll duplicates `util::proc::output_timeout` |  |
| `commands/editor.rs:1080` | prod | scp upload loop sleeps 400 ms between checks of cancel, child exit and a remote `stat` of the bytes so far (rate = bytes/dt) | C — progress sampling is the feature; exit and cancel latency rides on the tick (could be events); no tests |  |
| `commands/editor.rs:1210` | prod | Local file watcher is debounced 500 ms (notify_debouncer_mini) before `file-changed`/`file-deleted` | C — debounce; not injectable; no tests |  |
| `commands/editor.rs:1371` | prod | `git show` / `rev-parse` for a diff run under `output_timeout` 15 s | D |  |
| `commands/editor.rs:1415` | prod | `ssh test -d` bounded at 5 s → "SSH timed out" | D — child not `kill_on_drop`; a timeout leaks the ssh |  |
| `commands/editor.rs:1583` | prod | Remote quick-open listing over ssh bounded at 15 s | D — child not `kill_on_drop`; a timeout leaks the ssh |  |
| `commands/editor.rs:1621` | prod | Remote file watcher: every 3 s one batched ssh `stat` per host, emitting a change when the second-resolution mtime differs | A — a remote watcher stream (inotifywait/fswatch over the ControlMaster) could push changes | ⚠ seconds-resolution mtime: a second write in the same second as an observed one is never seen |
| `commands/editor.rs:1882` | test | Calls `identities_within` with 200 ms (then 100 ms) and asserts the two healthy paths answered and elapsed < 2 s, while the stuck path really waits out the deadline | C — the deadline is injected, but the healthy paths must answer on fresh threads within real 200 ms | ⚠ pass depends on two threads spawning and answering within 200 ms of real time |
| `commands/editor.rs:1896` | test | After releasing the stuck stat, polls every 5 ms (≤ 2 s) for its in-flight entry to clear, then re-identifies with a 2 s deadline | A — wait on the slot/condvar or join the stat thread instead of polling the map |  |
| `commands/relocate.rs:188` | prod | Abort RPC to every window with ABORT_TIMEOUT 5 s; the outcome is ignored | D |  |
| `commands/relocate.rs:216` | prod | Each window gets SUSPEND_TIMEOUT 90 s to suspend its tabs; a timeout aborts the move with nothing moved | D |  |
| `commands/scheduler.rs:96` | prod | Backup scheduler ticks every 60 s (first tick skipped); at l. 113 a backup is due when the newest backup file's mtime age + 5 s ≥ the interval | C — periodic check is the feature; clock and mtime read inline; no tests |  |
| `commands/scheduler.rs:145` | prod | Memory sampler ticks every 60 s: samples RSS and touches the crash marker | C — periodic tick is the feature; no tests |  |
| `commands/ssh_tunnel.rs:105` | prod | `cm_master_alive`: `ssh -O check` under a 3 s timeout; a timeout reads as "master not alive" | C |  |
| `commands/ssh_tunnel.rs:225` | prod | A dropped-tunnel breadcrumb is honoured only while < 30 min old (RECLAIM_BREADCRUMB_TTL) | C — TTL on `Instant`; not injected, tests backdate (see 1830) |  |
| `commands/ssh_tunnel.rs:341` | prod | `ssh_capture` kills its ssh after `secs` (10 s from callers) → None | D |  |
| `commands/ssh_tunnel.rs:421` | prod | `tend_tunnel` re-asks the master every 20 s to forward owed remote ports until all bound or the tunnel pid is gone | C — retry interval; tunnel death could be an event; no tests |  |
| `commands/ssh_tunnel.rs:766` | prod | Reclaim backoff sleeps 2, 5, 10 s (RECLAIM_BACKOFF_SECS) before re-asking for the lost remote port, hoping the remote sshd has reaped the dropped session, then walks to another port | C — retry backoff; `port_attempt_plan` is unit-tested without sleeping, the sleeps are not injectable | ⚠ sleep-and-hope in prod: an sshd that reaps later than ~17 s moves the port and strands every agent on the host |
| `commands/ssh_tunnel.rs:1396` | prod | SSH setup script bounded at 30 s → "SSH setup timed out" | D — child not `kill_on_drop`; a timeout leaks the ssh |  |
| `commands/ssh_tunnel.rs:1451` | prod | `probe_ssh_host` gives ssh 25 s to log in and run `true`; a timeout answers "not reachable" | D |  |
| `commands/ssh_tunnel.rs:1511` | prod | Waits ≤ 15 s for `ssh -v` to report the forward result, then errors (and the caller kills the child) | D |  |
| `commands/ssh_tunnel.rs:1830` | test | Tests the breadcrumb TTL by backdating `Instant::now() - (TTL + 1 s)` and `- (TTL − 60 s)` | C — no real wait, but the clock is not injected | ⚠ `Instant - 31 min` panics on a machine booted less than ~31 min ago (a fresh CI VM) |
| `commands/system.rs:73` | prod | Crash-report scan skips reports whose mtime is older than `max_age_days` (cutoff = now − days) | C — age window; days is a parameter, now read inline; diagnostic only |  |
| `commands/terminal.rs:244` | prod | Shell line probe writes an empty bracketed paste, then polls every 25 ms for up to 700 ms (twice) for the shell's OSC line report; None if none came | A — the OSC interceptor that sets `line_report` could notify a waiter | ⚠ prod sleep-and-poll: a loaded shell slower than 2 × 700 ms reads as "unknown line" |
| `commands/workspace.rs:70` | prod | Exit watchdog: 2 s after `exit_app`, a thread calls `app.exit(0)` again in case PTY threads stuck | D — note the expiry is a second graceful exit, not a hard kill |  |
| `commands/workspace.rs:3268` | prod | Backup trim deletes `aiterm_backup_*` files whose mtime age exceeds `backup_trim_age` (1 h … 1 y) | C — age threshold; clock inline; no tests |  |

#### mcp_proxy

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `mcp_proxy/mod.rs:583` | prod | An http server's watcher, after a failed list read, sleeps a backoff of 30 s doubling to 10 min (reset to 30 s on success) before trying again. | C |  |
| `mcp_proxy/mod.rs:602` | prod | When the upstream offers no `GET` stream, the watcher sleeps `REFRESH_EVERY` (1 h) and re-reads the list. | C |  |
| `mcp_proxy/mod.rs:617` | prod | Inside the stream `select!`, a 1 h `REFRESH_EVERY` tick sweeps idle sessions and re-reads the list even if the stream is quiet (missed by the sweep, same constant). | C |  |
| `mcp_proxy/mod.rs:627` | prod | When the change stream closes, a stream that lived under 5 s counts as "no stream" and waits 1 h; one that lived longer reconnects after 30 s. | C | ⚠ a time heuristic decides the server type: a real stream that drops within 5 s (slow network, a proxy that resets) is demoted to hourly, and its changes then wait up to an hour |
| `mcp_proxy/mod.rs:639` | prod | A stdio server's watcher sleeps 1 h and then re-lists a running child. | C |  |
| `mcp_proxy/mod.rs:670` | prod | `sweep_idle` gives back the upstream session of a downstream session whose wall-clock `last_used` is over 2 h old (`DOWN_IDLE_MS`). It reads `now_ms()` directly and has no test. | C |  |
| `mcp_proxy/mod.rs:811` | prod | A `tools/list` with no saved catalog waits up to 15 s for a first read, then answers with whatever is saved, which is an empty list if the read is slower. | C | ⚠ when the deadline passes, the agent gets an empty tool list without being told why; the later `list_changed` covers it only for runtimes that re-list |
| `mcp_proxy/oauth.rs:43` | prod | The OAuth HTTP client gives each request a 20 s timeout, then fails it. | D |  |
| `mcp_proxy/oauth.rs:139` | prod | `get_json` retries a 5xx or a timeout up to 3 times, sleeping 600 ms × attempt before each retry. | C |  |
| `mcp_proxy/oauth.rs:572` | prod | The sign-in callback listener wakes every 2 s (`CALLBACK_SWEEP`) to check whether any sign-in younger than `PENDING_TTL_MS` (10 min) still waits, and stops if none does. | A |  |
| `mcp_proxy/stdio.rs:595` | prod | One JSON-RPC request to a stdio child waits for the matching answer up to its bound (list 30 s, call 15 min, handshake `START_WAIT` 120 s or `VERIFY_WAIT` 180 s), then fails with "no answer within Ns". | D |  |
| `mcp_proxy/stdio.rs:808` | prod | A sweeper wakes every 30 s and stops each child no agent has used for `IDLE` (10 min). The clock is injected through `stop_idle(now, idle)`, and tests age `last_use` instead of waiting. | C |  |
| `mcp_proxy/stdio.rs:1009` | prod | After a child exits, `wait_exit` sleeps 50 ms "to let the stderr reader count the last lines" before it logs the count. | A | ⚠ a production sleep-and-hope, and `a_key...` asserts "2 stderr lines" in that log line, so a loaded machine can log fewer and fail the test. Awaiting the `drain_stderr` task fixes it |
| `mcp_proxy/stdio.rs:1028` | prod | A child that stayed up ≥ `STABLE` (60 s, real `Instant`) resets the restart backoff to its first step. | C |  |
| `mcp_proxy/stdio.rs:1070` | prod | The restart loop sleeps a backoff of 1 s doubling to 60 s, but only while the server was used within `IDLE` (`recently_used`, wall clock). Tests wait the real 1 s first step, through `eventually`. | C |  |
| `mcp_proxy/stdio_tests.rs:122` | test | `added()` verifies the fake with a 20 s handshake bound, and an expired bound fails the test (`unwrap`). | D |  |
| `mcp_proxy/stdio_tests.rs:151` | test | `eventually()` polls a condition (log line, `running()`, pid gone) every 100 ms for up to 10 s, then panics. | A |  |
| `mcp_proxy/stdio_tests.rs:332` | test | `a_failed_verify...` "mute.sh" never answers, so the test passes only once the real 2 s handshake timeout has run out. | C |  |
| `mcp_proxy/stdio_tests.rs:349` | test | A 5 s handshake bound that is never reached, because these cases fail before anything starts (no key, short key, key on argv). | D |  |
| `mcp_proxy/stdio_tests.rs:399` | test | `concurrent_calls...` sleeps 100 ms so that the slow call is in flight before the fast one. | B | ⚠ under load the order is not guaranteed, and the test still passes without having exercised the overlap |
| `mcp_proxy/stdio_tests.rs:478` | test | Waits up to 10 s for the `list_changed` notice on the downstream stream; `expect` fails the test on expiry. | D |  |
| `mcp_proxy/stdio_tests.rs:559` | test | `stopping_kills_the_whole_process_group` sleeps 200 ms hoping the slow tool's `sleep 1` grandchild exists before `stop`. | B | ⚠ two ways it goes wrong. If the call has not arrived when `stop` runs, it starts a new child and the `isError` assert fails (load-dependent). And the fake's grandchild exits after 1 s anyway, so "group gone" can pass without the group kill |
| `mcp_proxy/stdio_tests.rs:636` | test | `a_dropped_call...` drops a "hang" call through a 300 ms `timeout` and asserts it never answered (missed by the sweep). | B |  |
| `mcp_proxy/stdio_tests.rs:694` | test | `mid_start` sleeps 500 ms, hoping the queued call is waiting behind a start that the fake stretches to 2 s (`slowstart`), then stops it. | B | ⚠ load-dependent. If the queued call is not queued within 500 ms, or the 2 s start finishes first, "the queued call started a child" fails. Used by two tests |
| `mcp_proxy/upstream.rs:88` | prod | The http upstream client gives each connect a 10 s timeout. | D |  |
| `mcp_proxy/upstream.rs:150` | prod | Each upstream POST is bounded by `QUICK` (30 s, list and handshake) or `CALL` (15 min, tool call) and fails with "no answer within Ns". | D |  |
| `mcp_proxy/upstream.rs:217` | prod | The best-effort `DELETE` that ends a session is bounded at 30 s, and its result is ignored. | D |  |
| `mcp_proxy/upstream.rs:223` | prod | Opening the `GET` change stream is bounded at 30 s and returns an error on expiry. | D |  |

#### pty

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `pty/manager.rs:493` | prod | The PTY writer thread waits 100 ms on its command channel and, on each timeout, polls `child.try_wait()` to find out whether the shell exited. | A |  |
| `pty/manager.rs:601` | prod | The frame emitter sleeps `FRAME_INTERVAL` (16 ms) after each frame, which caps output at about 60 fps. | C |  |
| `pty/manager.rs:892` | prod | `resize_pty` counts output as "hot" if the last read was within 1000 ms (wall-clock `epoch_millis`), and hot resizes are deferred. `resize_plan` is pure and tested with a bool; the clock itself is not injected. | C |  |
| `pty/manager.rs:995` | prod | The resize applier thread wakes every 50 ms until `last_request` is 250 ms old (`RESIZE_DEBOUNCE_MS`), then applies the latest size. This is a trailing debounce built as a poll loop; there is no test. | C |  |
| `pty/manager.rs:1616` | prod | `invalidate_stale_ps_snapshot` keeps a `ps` snapshot younger than 50 ms for a "fresh" probe, assuming anything that young postdates the ssh transition. | A | ⚠ correctness rests on a timing assumption: a snapshot taken under 50 ms before ssh exec'd brings the stale-read bug back. Comparing the snapshot against the transition (a sequence number, or taken-after-event) fixes it |
| `pty/manager.rs:1630` | prod | `ps_rows_snapshot` serves the cached `ps` sweep while it is younger than `PROC_SNAPSHOT_TTL` (800 ms), so callers rate-limit `ps` spawns through it. | C |  |
| `pty/manager.rs:1839` | prod | `proc_tree_snapshot` serves the cached sysinfo sweep while it is younger than 800 ms. | C |  |
| `pty/ssh_watch.rs:73` | prod | `SshWatch::note` ignores a probe's ssh sighting made within `STALE_SIGHTING` (1000 ms) of the record being taken at a command end. | A | ⚠ a time window stands in for "was this snapshot taken before the exit". A probe served from a snapshot just over 1 s old still shows the dead ssh, and it could credit the next 255. Its test runs `note` right after `D` and depends on that taking under 1 s of real time |
| `pty/ssh_watch.rs:161` | prod | After a command start, a thread sleeps `START_PROBE_DELAY` (1200 ms), sized from the 800 ms snapshot TTL plus 400 ms, then probes the foreground for an ssh. | A | ⚠ a production sleep-and-hope coupled to another constant: the delay is correct only while TTL + margin holds. Probing on the exec or title event with a fresh sweep removes it |
| `pty/ssh_watch.rs:170` | prod | After an exit mark with an ssh on record, a thread sleeps `EXIT_CONFIRM_DELAY` (300 ms), "long enough for the prompt to finish drawing", then decides with a fresh sweep whether the ssh dropped. | A | ⚠ a production sleep-and-hope; under load the prompt may still be drawing. The prompt's own OSC 133 A, or the shell taking the tty back, is the event to wait for |

#### watch

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `watch.rs:385` | prod | `collect` skips a waiting script whose `expires_at` is before wall-clock `now_ms()` (TTL). | C |  |
| `watch.rs:441` | prod | The watch loop wakes every 5 s (`LOOP_SECS`, `MissedTickBehavior::Delay`) and re-scans every tab for script follow-ups to run. A newly approved script waits up to one tick. | C |  |
| `watch.rs:470` | prod | Within the loop, the script-file sweep runs at most every 60 s (`SWEEP_SECS`). | C |  |
| `watch.rs:482` | prod | A script is due only once its last start is at least its `every` old (15 s–24 h, default 60 s), measured on a real `Instant`. There is no injected clock; only `Run::view`'s stats are tested with fed times. | C |  |
| `watch.rs:634` | prod | `login_path` waits up to 5 s for `$SHELL -l` to print PATH, then falls back to a fixed system PATH and caches the result in a `OnceCell` for the rest of the launch. | C | ⚠ the fallback is silent and permanent: one slow login shell at startup (nvm, a loaded machine) leaves every watch script of the launch without `gh`/`jq`, and the scripts then report "broken" |
| `watch.rs:740` | prod | A local script runs with a kill-after-timeout of `c.timeout` (1–60 s, default 10); expiry kills the group and reports Broken "timed out". `a_timeout_is_broken...` waits the real 1 s. | C |  |
| `watch.rs:753` | prod | After the group is killed, stdout and stderr are each drained for up to 1 s. A process that escaped the group (setsid) and still holds a pipe yields empty output. | C |  |
| `watch.rs:861` | test | `a_timeout_is_broken...` asserts that a run with a 1 s timeout returned within 5 s of real time (missed by the sweep). | C | ⚠ the pass depends on machine load (fork, login PATH probe); it should be checked with an injected timeout or clock, not a wall-clock bound |
| `watch.rs:862` | test | It then sleeps 3 s and asserts that the `(sleep 2; touch)` grandchild never wrote its marker. | B | ⚠ proves a non-event by sleeping, and under load the 2 s grandchild may not have reached `touch` in 3 s even if alive. `killpg(pgid, 0)` reporting the group gone is the check |
| `watch.rs:871` | test | `a_background_child_does_not_outlive...` sleeps 2 s and asserts that the `(sleep 1; touch)` child never wrote its marker. | B | ⚠ a non-event proven by sleeping, with the same 1 s margin as above |
| `watch/remote.rs:95` | prod | The remote wrapper (shell, missed by the sweep) runs a watchdog that sleeps `{t}` (the script's timeout), then marks TIMEOUT and kills the script's group. The `run_local` tests wait it out for real (1 s). | C |  |
| `watch/remote.rs:194` | prod | The local `ssh` run is bounded at the script's timeout + `SSH_SLACK_SECS` (20 s); on expiry the host is reported Unreachable. | D |  |
| `watch/remote.rs:300` | test | `a_timeout_kills_the_whole_group` asserts the 1 s-timeout run returned within 10 s of real time (missed by the sweep). | C | ⚠ a wall-clock bound in a test; it depends on load |
| `watch/remote.rs:301` | test | It then sleeps 4 s and asserts the `(sleep 3; touch)` grandchild never wrote its flag. | B | ⚠ a non-event proven by sleeping, with a 1 s margin |
| `watch/remote.rs:313` | test | `a_finished_script_leaves_nothing_running` sleeps 4 s and asserts the `(sleep 3; touch)` child never wrote its flag. | B | ⚠ a non-event proven by sleeping, with a 1 s margin |
| `watch/remote.rs:351` | test | The `#[ignore]` `on_a_real_host` test sleeps 4 s after a timed-out run, then checks on the host that the grandchild's flag is absent. | B | ⚠ a non-event proven by sleeping, across ssh |

#### agent_requests

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `agent_requests.rs:220` | prod | `sweep(list, now)` lapses a pending request at its `expires_at` (PENDING_TTL 30 min, Hive/propose 24 h), fails an approved one never reported back (5 min; restart and Hive 30 min), forgets settled ones after 1 h | C — `now` is already a parameter and the tests (l. 697–754) feed values; no real wait |  |

#### browser_link

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `browser_link/mod.rs:681` | prod | A browser command waits ≤ 30 s (COMMAND_TIMEOUT) for the extension's reply on a oneshot, then removes the pending id and tells the agent it timed out | D |  |
| `browser_link/ws.rs:56` | prod | A new extension socket must send `hello` within 5 s (HELLO_TIMEOUT) or is dropped | D |  |
| `browser_link/ws.rs:143` | prod | Every 5 s (PRUNE_EVERY) the socket loop prunes links whose maiTerm tab has closed, detaching their Chrome tabs | A — a maiTerm tab close/archive is an event maiTerm raises itself; prune on it instead of on a tick |  |

#### comms

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `comms/mod.rs:925` | prod | Mattermost watcher ticks every 5 s (WATCH_INTERVAL_SECS) polling bound threads and monitored channels; per-channel error backoff counted in ticks (cap 60 ticks ≈ 5 min) | A — Mattermost's websocket event stream could push posts; the tick-counted backoff is C and would stay |  |
| `comms/mod.rs:1533` | prod | `prune_receipts(receipts, now)` drops released-thread receipts older than RECEIPT_TTL_MS (7 d) | C — `now` is a parameter and the test (l. 2068) feeds values |  |

#### host_guard

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `host_guard.rs:77` | prod | This computer's own addresses (read via the routing table) are cached and re-read when the cache is older than 30 s | C — cache TTL; `Instant` not injected; no test depends on it |  |

#### instance_lock

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `instance_lock.rs:77` | prod | Launch polls `try_lock` every 100 ms for up to 20 s (3 s when opening share files); past it, concludes another instance is running (`Taken`) | A — a relaunch is waiting for its parent to exit: wait on that pid or a blocking `lock()` on a thread, not on a poll with a fixed budget | ⚠ prod sleep-and-hope: an old instance whose shutdown takes > 20 s makes the relaunch give up and hand off to a process that is exiting |
| `instance_lock.rs:102` | test | `acquire_at` with 250 ms against a holder that never releases must return `Taken` | C — the wait budget is the behaviour, injected as a parameter; deterministic, but waits real 250 ms |  |
| `instance_lock.rs:110` | test | Releaser thread sleeps 300 ms before dropping the lock so the 5 s acquire has to poll | B — sleep-to-order: on a loaded machine the release can precede the acquire and the waiting path goes untested; signal "first WouldBlock seen" instead |  |

#### relocate

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `relocate/agents.rs:580` | prod | SQLite `busy_timeout` 5 s when rewriting a Codex thread's cwd; a still-busy DB becomes a warning | D |  |

#### share

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `share/git.rs:18` | prod | Local git queries (rev-parse, remote -v) run under `output_timeout` 15 s (LOCAL_TIMEOUT) | D — note `run_in` returns None on timeout, the same as "not a repo" |  |
| `share/git.rs:202` | prod | `git ls-remote` probe bounded at 20 s (PROBE_TIMEOUT); a timeout is reported as `Unverified` ("the clone may still work") | D |  |

#### stack

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `stack/ports.rs:94` | prod | `bindable` treats a port as unanswered when a loopback `connect_timeout` of 200 ms fails, timeout included | A — decide on the refusal (ECONNREFUSED) event; a timeout should read as "in use" | ⚠ a listener too busy to accept in 200 ms reads as free, and the SO_REUSEADDR bind beside a wildcard listener may then succeed: a double-booked port |

#### state

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `state/persistence.rs:667` | prod | `save_state` writes at once only if the last write was ≥ 750 ms ago (SAVE_COALESCE_MS), else parks the snapshot | C — coalescing; clock inline; `cfg!(test)` bypasses it, so the coalescing path is never tested |  |
| `state/persistence.rs:675` | prod | The deferred flush thread sleeps the rest of the 750 ms window, then writes the newest parked snapshot | C — debounce; same as 667 |  |
| `state/persistence.rs:722` | prod | Conflict guard: the state file's mtime (ms) newer than the last one this process recorded means another writer; re-baseline or write a conflict file (again at l. 762) | A — mtime is a clock used as a version; a generation/content hash would decide without the clock |  |
| `state/persistence.rs:835` | prod | The rolling `.bak.json` is refreshed only when ≥ 60 s (BACKUP_INTERVAL_MS) since the last | C — rate limit; clock inline; no tests |  |

#### util

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `util/proc.rs:112` | prod | `output_timeout` kills and reaps the child once its deadline passes → `TimedOut` | D |  |
| `util/proc.rs:114` | prod | Waiting for that child is a `try_wait` poll sleeping 1 ms doubling to 50 ms | A — child exit can be awaited (`wait()` on a thread + `recv_timeout`) |  |
| `util/proc.rs:125` | prod | Draining stdout/stderr is bounded by what is left of the same deadline (a grandchild may hold the pipes) | D |  |
| `util/proc.rs:137` | test | Tests give fast commands 5 s / 10 s / 1 s deadlines that should never fire | D — watchdogs whose expiry is a failure |  |
| `util/proc.rs:154` | test | `sleep 30` under a 200 ms deadline must time out, asserting elapsed < 5 s | C — kill-after-timeout tested with a real 200 ms wait (duration injected) |  |

#### windows_setup

| Site | Where | What | Class | Flag |
|---|---|---|---|---|
| `windows_setup.rs:381` | prod | `run_quiet` polls `try_wait` every 50 ms and kills the program past its limit (10 s for `claude --version`, 5 s for `wsl -l -v`) → None | D — the poll could be a wait; note None reads the same as "no version"/"no WSL" in the setup report |  |

## 4. The goal for the next Hive

**Make maiTerm's Rust code wait on events instead of on time: no production path sleeps and
hopes, no test sleeps to wait or to prove that something didn't happen, and every behaviour that
is about time takes its clock from something a test can feed.**

It is done when:

1. **Every A row in §3.4 waits on an event**: an ack, a channel, a condition, a sequence number,
   a process exit, an output or OSC event. No fixed sleep remains on its path. A row that turns
   out to have no event to wait on is re-classed in this doc, with the reason.
2. **Every B row is gone.** The test waits on the event, or asserts a decision the code now
   reports ("skipped: slow", "held: draft"), instead of sleeping.
3. **Every C row takes its durations or clock from an argument, a field or a trait that a test
   sets**, and its tests feed values instead of waiting real time. No test asserts a wall-clock
   upper bound (`elapsed() < x`), and no test builds an `Instant` by subtraction without
   `checked_sub`.
4. **Every D row is still only a net**: its expiry is reported as a failure, never as success and
   never as a silent fallback. Any D whose expiry passes or falls back is moved to A or C.
5. **The sweep agrees with the doc.** Re-running the §3.1 commands finds no site that §3.4 doesn't
   list, and §3.4 shows the current line numbers and classes.
6. **CI passes repeatedly under load**:
   - the Test workflow's Windows job passes ten consecutive runs (`workflow_dispatch`);
   - the whole `cargo test --locked` suite passes ten consecutive times on macOS, outside the Hive
     sandbox (§6.3), with every core kept busy by another process and `--test-threads` set to
     twice the core count.

   A failure in any of those runs is a defect to fix (§1), not a rerun.
7. **Reviewers flag new timing.** The reviewer brief that `getReviewBrief` hands out
   (`REVIEWER_BRIEF` in `claude_code/review_brief.rs`) and CLAUDE.md's Common Pitfalls both
   carry the §1 rule: a reviewer flags any new sleep, timeout, interval or time threshold, in code
   or tests, unless it is a C site with an injected clock or a D net.

## 5. Suggested waves

Workers are split by file so that no two edit the same one. Nobody edits this doc during wave 2:
each worker reports which rows it resolved, re-classed or found, and wave 3 updates §3.4.

### Wave 1: shared pieces (one worker, first)

Everything in wave 2 builds on these, so they land before it starts:

- **The clock seam.** Pick one shape for C sites and write it into §1, with one site converted as
  the example. Durations passed as arguments, or a small `Clock` trait a test replaces, both
  appear in the code already: `hive/deliver.rs`'s `wait_out` takes its limit as a function, and
  `hive/autonomy.rs`'s `digest_in` takes `now`.
- **Test helpers** in a new crate-level test module (not in `filetree/`):
  - a `wait_until(what, condition)` whose deadline fails with the name of what it waited for;
  - a watchdog like `filetree/ops_tests.rs`'s, which aborts a hung test and names its step.
- `util/proc.rs` (`output_timeout`, which most of the D rows use) and its tests.
- The two `Instant::now() - d` fixtures (§3.3).
- The review rule (§4, criterion 7): `claude_code/review_brief.rs` and CLAUDE.md.
- A repeat option for `.github/workflows/test.yml`, if criterion 6 needs one. One worker owns that
  file.

### Wave 2: one worker per row, in parallel

| Worker | Files |
|---|---|
| mailink typing | `mailink/mod.rs`, `mailink/draft_hold.rs` |
| mailink, the rest | `mailink/accounts.rs`, `follow_ups.rs`, `inline_image.rs`, `mirror.rs`, `rpc.rs`, `subagents.rs`, `transcript.rs` |
| claude_code server | `claude_code/server.rs`, `lockfile.rs`, `claude_mod.rs` |
| claude_code asks and inbox | `claude_code/mod_asks.rs`, `mod_inbox.rs` |
| hive delivery | `hive/deliver.rs`, `events.rs`, `finish.rs`, `finish/wake.rs`, `tools.rs`, `retire.rs`, `owed.rs`, `waiters.rs`, `quiet.rs`, `fuses.rs`, `autonomy.rs`, `pressure.rs` |
| hive jobs and landing | `hive/queue.rs`, `queue/tests.rs`, `run.rs`, `serve.rs`, `serve/tests.rs`, `rpc.rs`, `land/*` |
| hive, the rest | `hive/change.rs`, `settings.rs`, `git.rs`, `worktree.rs`, `trust.rs`, `launch.rs`, `create.rs`, `new_goal.rs`, `tool_setup.rs`, `registry_proxy.rs`, `feed.rs`, `probe.rs`, `media/whisper.rs`, `gallery/thumbs.rs`, `memory/*` |
| agent_files | `agent_files/*` |
| pty and watch | `pty/*`, `watch.rs`, `watch/*` |
| mcp_proxy | `mcp_proxy/*` |
| commands and the small modules | `commands/*`, `comms/`, `instance_lock.rs`, `stack/`, `state/`, `share/`, `relocate/`, `browser_link/`, `host_guard.rs`, `agent_requests.rs`, `windows_setup.rs` |

Two couplings cross those lines:

- `claude_code/server.rs:4858` caps a wait that `mailink/draft_hold.rs`'s `await_ready` does.
  The claude_code server worker takes it once the mailink typing worker's change has landed.
- `pty/ssh_watch.rs`'s `START_PROBE_DELAY` must stay above `pty/manager.rs`'s snapshot TTL. One
  worker owns both, so fix them together.

The mailink typing worker has the hardest job. Claude's TUI gives no ack for a paste, a keystroke
or a repaint, and several of those sites exist because of that. Its first step is to find what
can stand in for one: the mod's `started` report, a hook, the screen read back, or
`last_output_ms` moving. Where nothing can, the row stays C with its durations injected, and the
report says so.

### Wave 3: reconcile and prove (one worker)

- Re-run §3.1 and update §3.4 from the sweep and the wave 2 reports.
- Run criterion 6, and file every failure as a defect.
- Delete this doc's §3.3 list once it is empty.

## 6. Known traps

These cost earlier workers time. §6.1 and the CI facts were re-checked for this doc; the rest
were observed by earlier Hive workers between 2026-11-05 and 2026-11-14.

### 6.1 Running cargo through `hiveRun`

- `hiveRun` runs in the repo root, which has no `Cargo.toml`. Pass
  `--manifest-path src-tauri/Cargo.toml` to every `cargo test` and `cargo build`. Without it,
  cargo fails at once and the job still holds until its timeout.
- **A failing `cargo test` under `hiveRun` is never reported as failed**: it holds the job until
  its timeout and comes back "interrupted", which is no receipt. Passing runs end in seconds once
  the build is cached. Give a run that may fail a short timeout (about 300 s) and filter it
  narrowly, and take receipts from narrow passing runs.
- The last lines of the ⟦HIVE RUN⟧ message hold only part of a failure list. To see the rest,
  re-run with `--skip` for the failures already seen.

### 6.2 Tests the worker sandbox breaks

These fail in a worker's sandbox and pass outside it, so they are not your change:

- The full `cargo test --lib hive::` had 27 failures (hook_script, worktree, tools, spike15 and
  settings tests: ones that need what the sandbox refuses) and then hung to its timeout. Filter to
  the modules you touched, for example
  `cargo test --manifest-path src-tauri/Cargo.toml --lib -- hive::media hive::tool_setup`.
- The four `watch::remote` tests (`exit_codes_output_and_state_cross_over`,
  `quotes_and_a_tilde_folder_survive`, `a_timeout_kills_the_whole_group`,
  `a_finished_script_leaves_nothing_running`) fail under `hiveRun`: they fake an ssh host with
  `/bin/sh` and a scratch `HOME`. Add `--skip watch::remote` to any filter that matches `watch::`.
  Two of them are rows in §3.4: the pty and watch worker changes them, so the fix has to be
  checked outside the sandbox.
- `hive/probe.rs` is `#[cfg(all(test, unix))]` and every probe in it is `#[ignore]`d, run by hand
  with `cargo test --lib hive::probe -- --ignored`. Its rows are test rows.

### 6.3 What CI does and doesn't run

- `.github/workflows/test.yml` runs `cargo test --locked` on `windows-latest` only, with
  `timeout-minutes: 25` (a healthy run takes 8–11 minutes). There is no macOS or Linux job, so a
  `#[cfg(unix)]` test (most of `watch`, `util/proc.rs`, `commands/ssh_tunnel.rs`'s tests) never
  runs in CI. Criterion 6's macOS runs are the only check those get.
- `cargo check` on this Mac doesn't build the Windows code. A real Windows-target check works
  when the C dependencies (ring, aws-lc-sys, libsqlite3-sys) are given stub tools, since
  `cargo check` never links:
  - point `CC_x86_64_pc_windows_msvc` at a script that creates an empty file for each `-o`;
  - point `AR_x86_64_pc_windows_msvc` at one that writes `!<arch>\n` into the archive it names;
  - put an `llvm-rc` on `PATH` that writes `!<arch>\n` where `/fo` points;
  - then run `cargo check --tests --lib --locked --target x86_64-pc-windows-msvc`.

  Flipping `cfg(unix)` off natively is not a substitute: it missed 20 dead-code warnings. Run
  this check before handing back any change to code that differs on Windows.

### 6.4 Writing the fixes

- **ast-grep doesn't see into macros** (§3.1). A `sleep` in a `select!` arm or an `elapsed()` in
  an `assert!` won't show up in a pattern search.
- **File modification times are coarse.** Several tests prove "unchanged" or "later" with an
  mtime after a short sleep; on a filesystem with one-second mtimes that proves nothing.
- **Don't edit `src-tauri/src/filetree/`** as part of this goal. Its tests are being fixed on their
  own branch (§2.2); check whether that work has landed before deciding whether to touch it.

### 6.5 Found in passing, outside timing

Several tokio ssh children run under a timeout without `kill_on_drop(true)`, so a timeout leaves
the ssh process running: `commands/accounts.rs:2296`, `commands/editor.rs:1415` and `:1583`,
`commands/ssh_tunnel.rs:1396`, `mailink/mod.rs:4415` and `:4448`. The worker that owns each file
should fix it with the timing work there.
