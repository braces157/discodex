# Privacy and diagnostics verification

Verified on Windows on October 28, 2026 with Rust and Cargo 1.95.0.

## Commit order and checks

Each code commit passed `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --quiet` before the next unit was applied.

| Unit | Passed | Ignored | Purpose |
| --- | ---: | ---: | --- |
| Crush payload test | 154 | 1 | Check serialized Discord activity instead of internal events containing the checkout path. |
| Privacy controls | 157 | 1 | Require reasoning opt-in, hide names, and preserve privacy through partial config saves. |
| Worker extraction | 162 | 1 | Move ingestion and scheduling into focused owners with behavior tests. |
| Local diagnostics | 165 | 1 | Report source access, observed activity, fallback policy, watches, and Discord connection state. |

The ignored test is `sources::tests::live_sessions_produce_events`. It already requires locally installed providers and was not enabled for these checks.

The original writable-directory baseline was 153 passed, one failed, and one ignored. The failed Crush assertion matched words in an internal workspace path. Verification now uses a writable temporary directory whose path deliberately contains `Documents` and `Discodex`.

## Reproduce the checks

Run from the repository root in PowerShell. The directory below holds test fixtures.

```powershell
$previousTemp = $env:TEMP
$previousTmp = $env:TMP
$verificationTemp = Join-Path (Get-Location) 'target/verification/Documents/Discodex'
New-Item -ItemType Directory -Force $verificationTemp | Out-Null
try {
    $env:TEMP = $verificationTemp
    $env:TMP = $verificationTemp
    cargo fmt --check
    if ($LASTEXITCODE -ne 0) { throw 'Formatting failed' }
    cargo clippy --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
    cargo test --quiet
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
} finally {
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
}
```

The final code passed all four commands. The test suite reported 165 passed, zero failed, and one ignored. The release build completed successfully.

## Privacy regression evidence

`extracted_confidential_heading_requires_opt_in_in_every_payload` sends the heading `confidential-client acquisition` through provider parsing, activity state, and final Discord activity serialization. It covers every privacy-option combination in both thinking and tool phases, with positive checks that opted-in text and visible names are present.

Temporarily bypassing the redaction call in the final activity builder caused this test to fail with `left: true` and `right: false`. Restoring redaction made it pass. The mutation was not committed.

The Crush regression checks every emitted activity in a fixture containing private prompts, reasoning, tool bodies, commands, output, and answer text. It also checks that a legitimate filename reaches the payload.

Additional regressions cover partial config saves, retried malformed documents, document deduplication, incomplete log records, Discord retry and refresh deadlines, Aider-only wakeups, completed-session observation times, and external Crush database access failures.

## Diagnostic command evidence

`cargo run --quiet -- --diagnostics` exited successfully with an isolated configuration that disabled presence and focused-app detection and hid project and file names. The report included source access and fallback policy, omitted source paths, and reported that standalone diagnostics do not probe Discord.

The observed report began with these settings.

```text
Discodex diagnostics

Presence enabled: false
Discord: Not probed. Standalone diagnostics do not connect to Discord; use tray Diagnostics for the running connection
Focused-app detection enabled: false
Focused provider: None detected
Reasoning opt-in: false
Project names: false
File names: false
```

## Review and limits

An independent GPT-6 Astra review found no actionable privacy, correctness, deslop, or comment issues. The review checked the source and verification records. The primary agent ran the commands above.

Live tray interactions and Discord delivery were not exercised. An existing Discodex process was left running. Database access checks do not prove adapter schema compatibility. Document and database history uses the time Discodex observed it, which can differ from the provider's original event time.
