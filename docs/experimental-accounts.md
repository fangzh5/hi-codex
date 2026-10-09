# Optional account switching

Included in the main branch starting with `v0.4.0`, after development on `codex/experimental-account-switching`. Account management remains opt-in. The checklist below records scenarios requiring ongoing acceptance testing, not completed certification; automated tests do not substitute for real-account desktop verification.

## Boundaries

The original App Server quota path is retained. The accounts backend has no codex-auth, Node.js or codext runtime dependency. `AccountManager::list` reads only and does not create its store. Account failures are presented separately from quota status. Account secrets are never formatted into menu labels or diagnostics.

Codex uses the inherited `CODEX_HOME`; the native manager resolves that same home, falling back to `%USERPROFILE%/.codex`. Profiles on different machines or in WSL are not switched by this Windows prototype. The backend checks local config for unsupported storage modes, login restrictions and WSL before credential writes, and never overrides those settings. Enterprise-enforced policy and external credential sources still require environment-specific acceptance testing.

## Persistence and switching

The independent, versioned JSON store is encrypted as one user-scoped DPAPI blob at `CODEX_HOME/hi-codex/accounts.dpapi`. Account identity combines user and workspace IDs rather than email. JWT payloads supply display metadata only; they are not proof of token validity. Network validation and renewal remain Codex's responsibility.

On switching, save the outgoing account's current credentials (including rotated refresh tokens), persist an encrypted recovery backup and target snapshot, then replace `auth.json`. If the current auth changed while preparing the operation, abort before replacement. Re-read the resulting auth to catch immediate external changes. A store write failure prevents active-auth replacement. Local selection success is distinct from successful account access; the subsequent quota refresh verifies access.

Explicit last-switch recovery accepts a missing or syntactically malformed `auth.json`, but refuses to overwrite different, parseable JSON (including refreshed credentials). IO errors, oversized files, directories and reparse points still block recovery. Recovery from a missing path uses a no-replace move so a concurrently created credential file wins. If auth restoration succeeded but updating the encrypted store failed, the same restore can be retried.

Staging files use a protected owner/system DACL. Windows `ReplaceFileW` preserves the destination ACL and writes the prior file to a same-directory `.hicodex-backup` recovery path; the backend does not ignore ACL errors. A partial replacement failure attempts to restore this file only when the destination is missing. If rollback also fails, recovery and staging files remain. A missing encrypted-store primary can be read from its recovery copy without any passive writes; a corrupt primary is never silently replaced with an older copy. Completed writes remove the recovery copy. Crash leftovers may contain credentials and must be handled accordingly; this is not a claim of arbitrary power-loss durability.

A file handle with no sharing serializes HiCodex writers. This is not a lock honored by Codex or codex-auth: users must close Codex and other switchers before switching. The before/after checks reduce, but cannot eliminate, races with external writers.

## UI and refresh coordination

The right-click submenu offers Save current, Import JSON, Import codex-auth, saved accounts and Restore backup. Account operations run on a worker thread and hold the same gate as quota fetches. Cancelled or discarded refreshes set the existing pending flag while holding that gate, so even Save, Import and no-op switches resume an interrupted refresh. A successful switch clears old quota data before starting a new refresh. It does not restart any application.

The identity visibility toggle covers aliases, emails and workspace IDs. Account numbers remain visible and distinct; revealed labels include a short workspace ID, and switch confirmation names the selected entry. Taskbar centering records the original and last applied position, restores only its own changes, and respects subsequent external moves.

The locally selected account is checked and disabled in the menu. Switch and restore require one confirmation; successful operations use the existing flyout footer for eight-second feedback without an OK button. The flyout stays visible during that interval, then resumes normal hover behavior. Errors remain explicit modal messages. Quota refreshes do not erase transient operation feedback.

Imports preserve existing local snapshots, use registry aliases when available, and leave the active auth and codex-auth files untouched. Registry schema versions 2–4 are recognized for optional import; future versions require Codex-generated auth JSON supported by this parser. Auth JSON is an internal credential format, not a guaranteed stable public schema. codex-auth directory import skips invalid, unreadable, unsupported, unregistered and duplicate snapshots, reporting added/skipped totals. A malformed orphan cannot block valid registered accounts. Explicit JSON-file import (including arrays) remains all-or-nothing. To update an existing account's credentials, sign in through Codex and choose Save current account.

## Verification

Automated tests use synthetic JWTs and isolated temporary homes, never the user's real login. Coverage includes hidden aliases, distinct same-email workspace labels, partial directory import, all-or-nothing JSON import, retained duplicate credentials, missing/malformed-auth recovery, rejected new logins and inaccessible files, read-only missing-store fallback, DPAPI corruption, locked-file failure, injected partial replacement and rollback failures, concurrent file creation, and taskbar position ownership. These tests exercise Windows DPAPI and native file operations; rare replacement failures are injected at the native replacement boundary.

Manual acceptance checklist (not all scenarios have been verified):

1. Launch without saved accounts: quotas still refresh and no account directory is created.
2. Save a real file-mode ChatGPT account; restart HiCodex and verify its masked menu entry.
3. Import codex-auth snapshots; verify aliases and that source files and active auth are unchanged.
4. Close Codex, switch accounts, reopen it and verify the selected account works and quotas match.
5. Save/import while a quota refresh is pending; verify it resumes afterward. Switch while refreshing; old identity/quotas must not reappear.
6. Restore immediately after switching; verify the previous login returns. Using synthetic credentials, also verify missing/malformed-auth recovery. Change credentials externally and verify recovery refuses to overwrite them.
7. Corrupt a copied synthetic store; account management reports failure while quotas remain readable.
8. Test denied writes, long/Unicode paths, locked auth files, same-email workspaces and explicit unsupported credential modes.
9. Hide identities after importing aliases and verify neither aliases nor workspace IDs are shown. Reveal identity and confirm same-email workspaces can be distinguished.
10. Exit without enabling taskbar centering; its layout must remain unchanged. Enable/disable centering and verify the original position returns.

Current omissions: browser login inside HiCodex, alias editing, account deletion, keyring/API-key/WSL support, cross-machine encrypted-store migration, and seamless switching. These can be separate follow-up changes after the prototype is validated.
