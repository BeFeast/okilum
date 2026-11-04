# Unified Vaults settings (#2572)

Oleg chose option 1 on 2026-11-04: one **Vaults** sidebar entry. Its current
vault card keeps the existing vault controls, followed by **Other vaults**.
Current-vault list actions remain available under **More…** in its card;
other vaults keep their existing inline list actions. The known-vault history,
default preference and every vault setting retain their existing storage.

`show_vault` opens the card at the top, including when reusing an already
scrolled Settings window. Persisted section keys `vault` and `vaults` both
restore Vaults. UI-state reads and writes normalize the legacy singular key;
normal preference persistence writes the normalized value without a schema bump.

**Logs** starts with app-wide editor/regex preferences, followed by
**Log sources · <vault name>**, the vault's colour dot and an explicit storage
explanation. HTTPS/Kibana definitions and source-path mappings retain their
per-vault storage. With no open vault, source buttons are disabled and no
source-path editor is shown. Kubernetes logs remain in the application Logs
view; they do not define a persisted source list in Settings.

Reference mockup: <https://html.me.uk/t/okilum-settings-vaults/1-odna-sekciya-vaults.html>.
