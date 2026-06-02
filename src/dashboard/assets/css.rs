//! Shared dashboard CSS.

pub(crate) const DASHBOARD_CSS: &str = r#"
:root {
  color-scheme: dark;
  --bg: #0a0f19;
  --panel: #111827;
  --panel-2: #0f172a;
  --line: #1f2937;
  --text: #e5e7eb;
  --muted: #9ca3af;
  --accent: #38bdf8;
  --accent-2: #22d3ee;
}

* { box-sizing: border-box; }
body {
  margin: 0;
  font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif;
  background: radial-gradient(1200px 700px at 20% -10%, #13233f 0%, var(--bg) 52%);
  color: var(--text);
}

.app-shell {
  max-width: 1500px;
  margin: 0 auto;
  padding: 18px;
}

.app-header {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: flex-start;
  padding: 10px 2px 16px;
}

.app-header h1 {
  margin: 0;
  font-size: 1.45rem;
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
  display: flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
  font-size: 0.82rem;
  color: var(--muted);
  white-space: nowrap;
  user-select: none;
  min-width: 60px;
  padding: 8px 12px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--panel);
  flex-shrink: 0;
}

.live-toggle input:checked + span {
  color: var(--accent, #4fc3f7);
  font-weight: 600;
}

.controls input[type="search"],
.controls select {
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--panel);
  color: var(--text);
  padding: 11px 12px;
  font-size: 0.98rem;
  transition: border-color 0.15s;
}

.controls input[type="search"]:focus,
.controls select:focus {
  outline: none;
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgba(56, 189, 248, 0.15);
}

.layout {
  display: grid;
  grid-template-columns: minmax(250px, 0.95fr) 6px minmax(300px, 1.45fr);
  gap: 0;
  min-height: calc(100vh - 210px);
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
  background: rgba(56, 189, 248, 0.06);
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
  background: linear-gradient(180deg, var(--panel), var(--panel-2));
  overflow: hidden;
  min-width: 0;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
}

.list-pane {
  margin-right: 3px;
}

.detail-pane {
  margin-left: 3px;
}

.summary {
  padding: 12px 14px;
  color: var(--muted);
  border-bottom: 1px solid var(--line);
}

.result-list {
  max-height: calc(100vh - 320px);
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
  background: rgba(56, 189, 248, 0.08);
}

.result-item.active {
  background: rgba(34, 211, 238, 0.14);
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
  font-size: 1.08rem;
}

.detail-meta {
  margin: 5px 0 0;
  color: var(--muted);
  font-size: 0.86rem;
}

.detail-head button {
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--panel);
  color: var(--text);
  padding: 8px 10px;
  cursor: pointer;
}

.detail-head button:hover {
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
  background: #0b1220;
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
  color: #0a0f19;
  border-radius: 2px;
  padding: 0 2px;
  font-style: normal;
}

mark.hl-fuzzy {
  background: #fb923c;
  color: #0a0f19;
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

@media (max-width: 1020px) {
  .filter-row {
    grid-template-columns: 1fr;
  }

  .layout {
    grid-template-columns: 1fr !important;
    min-height: 0;
  }

  .resize-handle {
    display: none;
  }

  .list-pane,
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
