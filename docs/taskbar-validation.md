# Taskbar centering, recovery and display resizing

Version 0.4.1 moves centering into the native layout owner. Centering now sets the `cxHeader` of the existing native Rebar band containing `MSTaskSwWClass`. The band reserves leading space, and Explorer continues laying out the inner task list at its normal origin. A reset of the inner list to zero no longer removes the reservation. No spacer window, new band, external package or permanent fast polling worker is added. Header-space behavior is documented by [Microsoft's REBARBANDINFO reference](https://learn.microsoft.com/en-us/windows/win32/api/commctrl/ns-commctrl-rebarbandinfow).

The existing two-second recovery timer and scoped, coalesced Explorer object events remain. Events update the desired header when icon widths or available space change, with one trailing measurement after 120 ms of settling. Their role is to update layout inputs; they no longer pull the task list back after every reset. Hooks are removed when centering is disabled or HiCodex exits, and are reattached after Explorer restarts.

Button width and leading margin are retained across temporary MSAA failures for the same task-list window, parent and Explorer process. Mixed geometry is discarded if the list or its parent moves during measurement. Recreated windows do not inherit cached bounds. Widget movement is skipped when its geometry, visibility and stacking order are already correct. Geometry respects the band's original header, its child's minimum width, negative monitor coordinates and reserved right-side controls. Insufficient room restores the original native header. Restoration requires the same process, Rebar, band ID, child and last applied header; newer external values are preserved.

Rebar messages above `WM_USER` require explicit cross-process data marshaling. The backend allocates one non-executable `REBARBANDINFO` buffer in the native control's process and uses only band query/header-setting messages. It checks class, owner, target bitness, band identity and current header, uses the older SDK header-size structure length for common-control compatibility, and bounds calls with a timeout. The buffer and process handle are reused and released on normal restoration. Reentrant cleanup uses a nonblocking lock and defers release to a later recovery pass if a native call is active. A timed-out receiver may still hold its pointer: that one client is quarantined, its buffer is neither reused nor freed, and its owner reclaims the allocation on exit. It cannot allocate repeatedly for the same owner. Unsupported access does not fall back to moving the icon list. Enabling an unavailable native backend reports an error.

`HiCodex.exe --taskbar-diagnose` reads the current task-band geometry and header without changing layout. It does not read account credentials.

Accent records are bounded to the currently discovered taskbars and store only the owner process, original policy and last applied policy. Unchanged accent policies are not rewritten. Failed restorations are retried by the existing timer, including after the corresponding option has been disabled.

An unreadable original accent policy prevents modification. Restore never overwrites a different current policy or a new Explorer process. On the next enabled apply, an external style becomes the new baseline. Actual simultaneous external writes cannot be fully excluded. Normal exit attempts restoration; forced termination cannot run cleanup.

Horizontal bounds include offscreen buttons so that reducing the desktop size does not center just the remaining visible subset. DPI changes trigger a layout refresh; the existing timer also checks for delayed Explorer layout changes after an RDP transition.

## Tests

- Layout calculations across 640–3840 pixel desktops, negative monitor coordinates, multiple simulated scale factors and icon-group widths.
- Recovery records retained on failed reads/writes, removed on success, and ignored for external styles or changed Explorer process identities.
- Native band ownership tests for repeated centering and externally changed/recreated bands.
- Transient button-read failures retain the centered target; changed parent/window/process identities invalidate the cache.
- A native message-only test window verifies that a burst of 40 layout notifications produces one queued update, processing rearms the queue, disabling stops updates, and destruction invalidates cached bounds. The test never moves the real taskbar.
- A separate process owns a hidden native Rebar fixture. The integration test reproduces the old list-offset reset, then verifies 50 list resets and simulated notification-area width changes with the new header reservation. Assertions run before any corrective callback or timer. Restoration and refusal to overwrite a newer header are also checked.
- A deliberately delayed fixture verifies that timed-out messages keep their data buffer alive, cannot reuse it, and do not crash the receiver when it resumes.

These are automated logic checks, not a real RDP session or Windows-version compatibility certification. Before release, manually verify:

1. Connect/disconnect RDP repeatedly and resize its desktop, including 640/800/1024 pixel widths and 100/150/200% scaling. Icons must remain reachable; insufficient width must fall back to non-centered layout.
2. Add/remove a monitor; restart Explorer; check accent styles and main-taskbar positioning recover without growing saved-window records.
3. Change theme or use another taskbar tool, then disable HiCodex effects; do not overwrite a newer external style during restore.
4. Disable/re-enable each effect repeatedly and exit normally. Check both monitors recover and no stale theme is restored.
5. Compare idle CPU, private bytes, working set and handles over at least 30 minutes with the previous build. Short snapshots alone are not a leak or performance test.
6. Repeatedly start/exit programs with notification icons, and add/remove taskbar buttons. Check that the reserved native space remains during resets, and verify normal icon access, widget visibility and restoration after disabling centering.

Secondary-taskbar icon centering remains unsupported. The low-frequency timer remains necessary as a fallback for taskbar/remote-session changes not delivered directly to the child widget. A failed restoration cannot be retried after the application has exited.
