# Taskbar recovery and display resizing (v0.4.0)

This patch preserves the native implementation and the existing two-second timer; no new polling thread, dependency or offscreen taskbar renderer is introduced. Accent records are bounded to the currently discovered taskbars and store only the owner process, original policy and last applied policy. Unchanged accent policies are not rewritten. Failed restorations are retried by the existing timer, including after the corresponding option has been disabled.

An unreadable original accent policy prevents modification. Restore never overwrites a different current policy or a new Explorer process. On the next enabled apply, an external style becomes the new baseline. Actual simultaneous external writes cannot be fully excluded. Normal exit attempts restoration; forced termination cannot run cleanup.

Centering uses the intersection of the task-list parent client area, taskbar and reserved right-side controls. Overflowing icon groups are not centered. Horizontal bounds include offscreen buttons so that reducing the desktop size does not center just the remaining visible subset. DPI changes trigger a layout refresh; the existing timer also checks for delayed Explorer layout changes after an RDP transition.

## Tests

- Layout calculations across 640–3840 pixel desktops, negative monitor coordinates, multiple simulated scale factors and icon-group widths.
- Recovery records retained on failed reads/writes, removed on success, and ignored for external styles or changed Explorer process identities.
- Existing ownership tests for repeated centering and externally moved/recreated task-list windows.

These are automated logic checks, not a real RDP session or Windows-version compatibility certification. Before release, manually verify:

1. Connect/disconnect RDP repeatedly and resize its desktop, including 640/800/1024 pixel widths and 100/150/200% scaling. Icons must remain reachable; insufficient width must fall back to non-centered layout.
2. Add/remove a monitor; restart Explorer; check accent styles and main-taskbar positioning recover without growing saved-window records.
3. Change theme or use another taskbar tool, then disable HiCodex effects; do not overwrite a newer external style during restore.
4. Disable/re-enable each effect repeatedly and exit normally. Check both monitors recover and no stale theme is restored.
5. Compare idle CPU, private bytes, working set and handles over at least 30 minutes with the previous build. Short snapshots alone are not a leak or performance test.

Secondary-taskbar icon centering remains unsupported. The low-frequency timer remains necessary as a fallback for taskbar/remote-session changes not delivered directly to the child widget. A failed restoration cannot be retried after the application has exited.
