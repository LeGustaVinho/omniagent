# Managed worktree fixtures

The same-agent, same-branch isolation case in `managed_worktrees.rs` adapts Armada's
[MissionDockIsolationSuite](https://github.com/jchristn/Armada/blob/bc1114bc72dfb491793f02ddf9bb2fff381d1bb2/src/Test.Shared/Suites/E2E/MissionDockIsolationSuite.cs).
It uses disposable repositories and SQLite to check that saved and live evidence
resolve to the exact project, checkout, and attempt when branch names match.

The cancellation and completion case adapts the first terminal transition rule in
[Armada's JobService](https://github.com/jchristn/Armada/blob/bc1114bc72dfb491793f02ddf9bb2fff381d1bb2/src/Armada.Core/Services/JobService.cs#L79-L115).
It checks both provider outcomes after an interrupt request, then replays a stale
interrupt and a conflicting terminal outcome against the persisted attempt. A later
writer can acquire the reservation after settlement while the saved candidate
remains attached to the original attempt.

The upstream MIT notice is included in `LICENSE-Armada`.
