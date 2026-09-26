//! Shared dashboard CSS.

pub(crate) const DASHBOARD_CSS: &str = r#"
:root {
  color-scheme: dark;
  --bg: #0e0e0e;
  --panel: #161616;
  --panel-2: #1e1e1e;
  --line: rgba(255, 255, 255, 0.16);
  --text: #f5f1e7;
  --muted: rgba(245, 241, 231, 0.64);
  --accent: #3d7a72;
  --accent-2: #3d7a72;
  --danger: #b86a5c;
  --font-display: "Instrument Serif", "Iowan Old Style", Palatino, Georgia, serif;
  --font-body: Inter, system-ui, sans-serif;
  --font-mono: "JetBrains Mono", ui-monospace, monospace;
}

* { box-sizing: border-box; }
html, body { height: 100%; }
body {
  margin: 0;
  overflow: hidden;
  font-family: var(--font-body);
  background: var(--bg);
  color: var(--text);
  line-height: 1.5;
}

.app-shell {
  height: 100vh;
  height: 100dvh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  margin: 0;
  padding: 0;
}

.app-header {
  position: sticky;
  top: 0;
  z-index: 30;
  flex: 0 0 auto;
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: center;
  width: 100%;
  padding: 12px 18px;
  background: var(--bg);
  border-bottom: 1px solid var(--line);
}

.brand-lockup {
  display: flex;
  align-items: center;
  gap: 16px;
  min-width: 0;
}

.brand-pair {
  display: flex;
  align-items: center;
  gap: 0.55rem;
}

.brand-word {
  font-family: var(--font-display);
  font-weight: 400;
  font-size: 1.35rem;
  line-height: 1;
  letter-spacing: 0.04em;
  color: var(--text);
}

.brand-mark,
#aicx-mark {
  display: block;
  flex: none;
  width: 28px;
  height: 28px;
  margin: 0;
  color: var(--text);
  background: transparent;
}

.app-header h1,
.detail-head h2 {
  font-family: var(--font-display);
  font-weight: 400;
}

.meta {
  margin: 4px 0 0;
  color: var(--muted);
  font-size: 0.9rem;
}

.header-stats {
  display: grid;
  grid-template-columns: repeat(3, minmax(90px, 1fr));
  gap: 8px;
}

.stat {
  border: 1px solid var(--line);
  background: var(--panel);
  border-radius: 10px;
  padding: 8px 10px;
  text-align: right;
}

.stat strong {
  display: block;
  font-size: 1.1rem;
}

.stat span {
  color: var(--muted);
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.controls {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
}

.search-row {
  display: flex;
  gap: 10px;
  align-items: center;
}

.search-row input[type="search"] {
  flex: 1;
  min-width: 0;
}

.filter-row {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
}

.live-toggle {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  cursor: pointer;
  font-size: 0.84rem;
  color: var(--text);
  white-space: nowrap;
  user-select: none;
  box-sizing: border-box;
  height: 2.25rem;
  padding: 0 0.75rem;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: transparent;
  flex-shrink: 0;
}

.time-row {
  display: flex;
  gap: 6px;
  align-items: center;
}

.time-btn,
.regen-btn,
.studio-card button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  box-sizing: border-box;
  height: 2.25rem;
  margin: 0;
  padding: 0 0.75rem;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: transparent;
  color: var(--text);
  font-family: inherit;
  font-size: 0.84rem;
  font-weight: 400;
  line-height: 1;
  cursor: pointer;
}

.time-btn {
  flex: 1 1 0;
  min-width: 0;
}

.time-btn:hover,
.regen-btn:hover,
.studio-card button:hover,
.live-toggle:hover {
  border-color: var(--accent);
}

.time-btn.active {
  border-color: var(--accent);
  color: var(--text);
  font-weight: 600;
}

.live-toggle input:checked + span {
  color: var(--accent);
  font-weight: 600;
}

.controls input[type="search"],
.controls select {
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--panel);
  color: var(--text);
  padding: 14px 16px;
  font-size: 1.05rem;
  min-height: 48px;
  transition: border-color 160ms ease;
}

.controls input[type="search"]:focus,
.controls select:focus {
  outline: none;
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgba(61, 122, 114, 0.45);
}

.layout {
  flex: 1 1 auto;
  min-height: 0;
  display: grid;
  grid-template-columns: minmax(250px, 0.95fr) 6px minmax(300px, 1.45fr);
  grid-template-rows: minmax(0, 1fr);
  gap: 0;
  align-items: stretch;
}

.main-column {
  min-width: 0;
  min-height: 0;
  overflow: auto;
  padding: 12px 14px 18px;
}

.resize-handle {
  width: 6px;
  cursor: col-resize;
  position: relative;
  z-index: 10;
  background: transparent;
  transition: background 0.15s;
}

.resize-handle::after {
  content: '';
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  width: 2px;
  height: 48px;
  border-radius: 2px;
  background: var(--line);
  transition: background 0.15s, height 0.15s;
}

.resize-handle:hover,
.resize-handle.dragging {
  background: rgba(61, 122, 114, 0.16);
}

.resize-handle:hover::after,
.resize-handle.dragging::after {
  background: var(--accent);
  height: 72px;
}

.list-pane,
.detail-pane {
  border: 1px solid var(--line);
  border-radius: 12px;
  background: var(--panel);
  overflow: hidden;
  min-width: 0;
}

.list-pane {
  position: sticky;
  top: 0;
  align-self: stretch;
  height: 100%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  margin: 12px 0 12px 14px;
  overflow: hidden;
}

.summary {
  flex: 0 0 auto;
  padding: 12px 14px;
  color: var(--muted);
  border-bottom: 1px solid var(--line);
}

.result-list {
  flex: 1 1 auto;
  min-height: 0;
  overflow: auto;
}

.result-item {
  width: 100%;
  text-align: left;
  border: 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
  background: transparent;
  color: inherit;
  padding: 11px 13px;
  cursor: pointer;
  transition: background 0.12s;
}

.result-item:hover {
  background: rgba(61, 122, 114, 0.12);
}

.result-item.active {
  background: rgba(61, 122, 114, 0.2);
}

.result-top {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

.badge {
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 2px 8px;
  font-size: 0.72rem;
  color: var(--muted);
}

.result-name {
  margin-top: 6px;
  font-size: 0.88rem;
}

.result-preview {
  margin-top: 6px;
  color: var(--muted);
  font-size: 0.8rem;
  line-height: 1.35;
  white-space: pre-wrap;
}

.detail-pane {
  display: flex;
  flex-direction: column;
  margin: 0;
}

.detail-head {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  align-items: flex-start;
  padding: 13px 14px;
  border-bottom: 1px solid var(--line);
}

.detail-head h2 {
  margin: 0;
  font-size: 1.35rem;
  letter-spacing: 0.02em;
}

.detail-meta {
  margin: 5px 0 0;
  color: var(--muted);
  font-size: 0.86rem;
}

.detail-actions {
  display: flex;
  gap: 6px;
}

.detail-head button,
.detail-actions button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex: 1 1 0;
  box-sizing: border-box;
  height: 2.25rem;
  margin: 0;
  padding: 0 0.75rem;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: transparent;
  color: var(--text);
  font-family: inherit;
  font-size: 0.84rem;
  font-weight: 400;
  line-height: 1;
  white-space: nowrap;
  cursor: pointer;
}

.detail-head button:hover,
.detail-actions button:hover {
  border-color: var(--accent);
}

.detail-path,
.detail-preview {
  padding: 0 14px;
  margin: 10px 0 0;
  color: var(--muted);
  font-size: 0.86rem;
}

.detail-preview {
  color: var(--text);
}

.detail-content {
  margin: 10px 14px 12px;
  border: 1px solid var(--line);
  background: #0e0e0e;
  border-radius: 10px;
  padding: 12px;
  overflow: auto;
  white-space: pre-wrap;
  line-height: 1.35;
  font-size: 0.86rem;
  flex: 1;
  min-height: 280px;
}

mark.hl {
  background: #facc15;
  color: #0e0e0e;
  border-radius: 2px;
  padding: 0 2px;
  font-style: normal;
}

mark.hl-fuzzy {
  background: #fb923c;
  color: #0e0e0e;
  border-radius: 2px;
  padding: 0 2px;
  font-style: normal;
}

.assumptions {
  margin: 0 14px 14px;
  color: var(--muted);
}

.assumptions ul {
  margin: 8px 0 0;
  padding-left: 18px;
}

.empty {
  padding: 16px;
  color: var(--muted);
}

/* Human Bearer login overlay (server-mode only; static dashboard has no /api/* gate) */
.aicx-auth-overlay {
  position: fixed;
  inset: 0;
  z-index: 10000;
  display: none;
  align-items: center;
  justify-content: center;
  background: rgba(14, 14, 14, 0.82);
  padding: 18px;
}
.aicx-auth-card {
  width: min(420px, 100%);
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 12px;
  padding: 28px 24px;
  box-shadow: 0 18px 50px rgba(0, 0, 0, 0.45);
}
.aicx-auth-card h2 {
  margin: 0 0 10px;
  font-size: 1.2rem;
  color: var(--accent);
}
.aicx-auth-card p {
  margin: 0 0 16px;
  color: var(--muted);
  font-size: 0.9rem;
  line-height: 1.45;
}
.aicx-auth-card code {
  color: var(--accent-2);
  font-size: 0.85em;
}
.aicx-auth-err {
  color: #f87171;
  font-size: 0.85rem;
  margin: 0 0 12px;
}
.aicx-auth-card input[type="password"] {
  width: 100%;
  padding: 10px 12px;
  margin: 0 0 12px;
  border-radius: 8px;
  border: 1px solid var(--line);
  background: var(--panel-2);
  color: var(--text);
  font-size: 0.95rem;
}
.aicx-auth-card button[type="submit"] {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 100%;
  height: 2.25rem;
  padding: 0 0.75rem;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: transparent;
  color: var(--text);
  font-weight: 400;
  cursor: pointer;
}
.aicx-auth-card button[type="submit"]:hover {
  border-color: var(--accent);
}
.aicx-auth-card button[type="submit"]:disabled {
  opacity: 0.6;
  cursor: wait;
}

.studio { display: grid; gap: 12px; margin: 0 0 16px; }
.studio-card { border: 1px solid var(--line); background: var(--panel); border-radius: 14px; padding: 16px; }
.studio-card summary { min-height: 44px; cursor: pointer; }
#ctx-phrases { width: 100%; margin: 8px 0; background: var(--bg); color: var(--text); border: 1px solid var(--line); border-radius: 10px; font-family: var(--font-mono); font-size: 0.85rem; line-height: 1.45; }
#ctx-index { border-color: var(--accent); }

@media (max-width: 1020px) {
  .filter-row {
    grid-template-columns: 1fr;
  }

  .layout {
    grid-template-columns: 1fr !important;
    min-height: 0;
    overflow: auto;
  }

  .resize-handle {
    display: none;
  }

  .list-pane {
    position: sticky;
    top: 0;
    height: auto;
    max-height: 50vh;
    margin: 12px;
  }

  .main-column {
    height: auto;
    overflow: visible;
  }

  .detail-pane {
    margin: 0;
  }

  .result-list {
    max-height: 360px;
  }

  .detail-content {
    min-height: 220px;
  }
}
"#;
