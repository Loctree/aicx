//! Server-mode dashboard browser JavaScript.

/// JavaScript for the server-mode shell — all data fetched via API.
/// v6: real filters, sort, inline markdown renderer, URL state, expand/collapse.
pub(crate) const DASHBOARD_SERVER_SCRIPT: &str = r#"
(() => {
  const $ = (id) => document.getElementById(id);
  const ui = {
    search: $('ctx-search'), project: $('ctx-project'), agent: $('ctx-agent'),
    kind: $('ctx-kind'), sort: $('ctx-sort'), score: $('ctx-score'),
    scoreLabel: $('ctx-score-label'), summary: $('ctx-summary'), list: $('ctx-list'),
    detailTitle: $('ctx-detail-title'), detailMeta: $('ctx-detail-meta'),
    detailContent: $('ctx-detail-content'), assumptions: $('ctx-assumptions'),
    copyPath: $('ctx-copy-path'), expand: $('ctx-expand'),
    genInfo: $('ctx-gen-info'), statFiles: $('ctx-stat-files'),
    statProjects: $('ctx-stat-projects'), statDays: $('ctx-stat-days'),
    regenerateBtn: $('ctx-regenerate'),
  };

  const hooks = { beforeRender: [], afterRender: [], onSelect: [] };
  const state = {
    query: '', project: '', agent: '', kind: '', sort: 'newest', since: '',
    scoreMin: 0, limit: 350, selectedId: null, rows: [], selectedRecord: null,
    browseRecords: [], mode: 'browse', expanded: false,
  };

  const renderMarkdown = AicxMarkdown.renderMarkdown;

  /* --- URL state --------------------------------------------------------- */
  const pushUrlState = () => {
    const p = new URLSearchParams();
    if (state.query) p.set('q', state.query);
    if (state.project) p.set('project', state.project);
    if (state.agent) p.set('agent', state.agent);
    if (state.kind) p.set('kind', state.kind);
    if (state.sort !== 'newest') p.set('sort', state.sort);
    if (state.since) p.set('since', state.since);
    if (state.scoreMin > 0) p.set('score', String(state.scoreMin));
    const qs = p.toString();
    const url = qs ? '?' + qs : location.pathname;
    history.replaceState(null, '', url);
  };
  const readUrlState = () => {
    const p = new URLSearchParams(location.search);
    if (p.has('q')) { state.query = p.get('q'); ui.search.value = state.query; }
    if (p.has('project')) { state.project = p.get('project'); ui.project.value = state.project; }
    if (p.has('agent')) { state.agent = p.get('agent'); ui.agent.value = state.agent; }
    if (p.has('kind')) { state.kind = p.get('kind'); ui.kind.value = state.kind; }
    if (p.has('sort')) { state.sort = p.get('sort'); ui.sort.value = state.sort; }
    if (p.has('since')) { state.since = p.get('since'); setTimeBtnActive(state.since); }
    if (p.has('score')) { state.scoreMin = parseInt(p.get('score'), 10) || 0; ui.score.value = state.scoreMin; ui.scoreLabel.textContent = state.scoreMin; }
  };

  /* --- helpers ----------------------------------------------------------- */
  const fillSelect = (node, values) => {
    const cur = node.value;
    while (node.options.length > 1) node.remove(1);
    values.forEach((v) => { const o = document.createElement('option'); o.value = v; o.textContent = v; node.appendChild(o); });
    if (cur) node.value = cur;
  };
  const runHooks = (name, value) => {
    const list = hooks[name] || [];
    return list.reduce((acc, fn) => { try { const m = fn(acc, null, state); return m === undefined ? acc : m; } catch (_) { return acc; } }, value);
  };
  const escapeHtml = (text) => { const d = document.createElement('div'); d.appendChild(document.createTextNode(text)); return d.innerHTML; };
  const normalizeText = (text) => {
    const map = {'\u0104':'A','\u0105':'a','\u0106':'C','\u0107':'c','\u0118':'E','\u0119':'e','\u0141':'L','\u0142':'l','\u0143':'N','\u0144':'n','\u00D3':'O','\u00F3':'o','\u015A':'S','\u015B':'s','\u0179':'Z','\u017A':'z','\u017B':'Z','\u017C':'z'};
    return text.replace(/[\u0104\u0105\u0106\u0107\u0118\u0119\u0141\u0142\u0143\u0144\u00D3\u00F3\u015A\u015B\u0179\u017A\u017B\u017C]/g, function(c) { return map[c] || c; }).toLowerCase();
  };
  const escapeRegex = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const highlightTerms = (text, query) => {
    if (!query || !text) return escapeHtml(text || '');
    const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return escapeHtml(text);
    const kinds = new Array(text.length).fill('');
    const markRange = (start, len, cls, ow) => { const end = Math.min(text.length, start + len); for (let i = start; i < end; i++) { if (ow || !kinds[i]) kinds[i] = cls; } };
    terms.forEach(function(term) { const re = new RegExp(escapeRegex(term), 'gi'); let m; while ((m = re.exec(text)) !== null) { if (!m[0]) break; markRange(m.index, m[0].length, 'hl', true); } });
    const normalizedText = normalizeText(text);
    terms.map(normalizeText).filter(Boolean).forEach(function(term) { let sf = 0; while (sf < normalizedText.length) { const idx = normalizedText.indexOf(term, sf); if (idx === -1) break; markRange(idx, term.length, 'hl-fuzzy', false); sf = idx + Math.max(term.length, 1); } });
    let html = ''; let start = 0;
    while (start < text.length) { const cls = kinds[start]; let end = start + 1; while (end < text.length && kinds[end] === cls) end++; const chunk = escapeHtml(text.slice(start, end)); html += cls ? '<mark class="' + cls + '">' + chunk + '</mark>' : chunk; start = end; }
    return html;
  };

  /* --- detail pane ------------------------------------------------------- */
  const renderDetail = (record, score) => {
    state.selectedRecord = record || null;
    state.expanded = false;
    if (ui.expand) ui.expand.textContent = 'Expand';
    if (!record) {
      ui.detailTitle.textContent = 'No result selected';
      ui.detailMeta.textContent = '';
      ui.detailContent.innerHTML = 'Use search or filters to pick a note.';
      return;
    }
    const title = record.file_name || record.file || '(unnamed)';
    const scoreTxt = typeof score === 'number' && score > 0 ? 'score ' + score + '/100' : '';
    const meta = [record.project, record.agent, record.kind, record.date, scoreTxt].filter(Boolean).join(' \u2022 ');
    ui.detailTitle.innerHTML = highlightTerms(title, state.query);
    ui.detailMeta.innerHTML = highlightTerms(meta, state.query);
    const previewText = record.preview || record.excerpt || '';
    if (previewText) {
      ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(previewText) + '</div>';
    } else {
      ui.detailContent.textContent = '(no preview)';
    }
  };

  const expandDetail = () => {
    const rec = state.selectedRecord;
    if (!rec) return;
    if (state.expanded) {
      renderDetail(rec, 0);
      return;
    }
    ui.detailContent.textContent = 'Loading full content\u2026';
    const endpoint = rec.id !== undefined ? '/api/chunk?id=' + rec.id : '/api/detail?id=' + rec.id;
    fetch(endpoint)
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (!data.ok) { ui.detailContent.textContent = 'Failed: ' + (data.error || 'unknown'); return; }
        const content = data.content || data.detail_text || '';
        state.expanded = true;
        if (ui.expand) ui.expand.textContent = 'Collapse';
        ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(content) + '</div>';
      })
      .catch(function(err) { ui.detailContent.textContent = 'Load failed: ' + err.message; });
  };

  /* --- result list ------------------------------------------------------- */
  const mkBadge = (txt) => { const n = document.createElement('span'); n.className = 'badge'; n.innerHTML = highlightTerms(String(txt || ''), state.query); return n; };
  const renderList = (rows) => {
    ui.list.innerHTML = '';
    if (!rows.length) {
      const e = document.createElement('div'); e.className = 'empty'; e.textContent = 'No records match current query/filters.';
      ui.list.appendChild(e); renderDetail(null, 0); return;
    }
    const visible = rows.slice(0, state.limit);
    const idKey = (r) => r.id !== undefined ? r.id : r.path;
    if (!state.selectedId || !visible.some(function(r) { return idKey(r.record) === state.selectedId; })) {
      state.selectedId = idKey(visible[0].record);
    }
    visible.forEach(function(entry) {
      const record = entry.record; const score = entry.score;
      const item = document.createElement('button'); item.type = 'button';
      const rid = idKey(record);
      item.className = 'result-item' + (rid === state.selectedId ? ' active' : '');
      const top = document.createElement('div'); top.className = 'result-top';
      top.appendChild(mkBadge(record.project || 'project'));
      top.appendChild(mkBadge(record.agent || 'agent'));
      top.appendChild(mkBadge(record.kind || 'kind'));
      top.appendChild(mkBadge(record.date || ''));
      if (typeof score === 'number' && score > 0) top.appendChild(mkBadge(score + '/100'));
      const name = document.createElement('div'); name.className = 'result-name';
      const fname = (record.file_name || record.file || '(unnamed)') + (record.size_human ? ' \u2022 ' + record.size_human : '');
      name.innerHTML = highlightTerms(fname, state.query);
      item.appendChild(top); item.appendChild(name);
      const previewText = record.excerpt || record.preview || '';
      if (previewText) {
        const preview = document.createElement('div'); preview.className = 'result-preview';
        const maxLen = 240; const truncated = previewText.length > maxLen ? previewText.slice(0, maxLen) + '\u2026' : previewText;
        preview.innerHTML = highlightTerms(truncated, state.query);
        item.appendChild(preview);
      }
      item.addEventListener('click', function() {
        state.selectedId = rid; renderList(state.rows); renderDetail(record, score); runHooks('onSelect', record);
      });
      ui.list.appendChild(item);
    });
    const sel = visible.find(function(r) { return idKey(r.record) === state.selectedId; }) || visible[0];
    if (sel) renderDetail(sel.record, sel.score);
  };

  /* --- browse + search --------------------------------------------------- */
  const applyBrowseFilters = () => {
    state.mode = 'browse';
    let rows = state.browseRecords
      .map(function(r) { return { record: r, score: 0 }; });
    const sortDir = state.sort;
    if (sortDir === 'oldest') rows.sort(function(a, b) { return (a.record.sort_ts || 0) - (b.record.sort_ts || 0); });
    else rows.sort(function(a, b) { return (b.record.sort_ts || 0) - (a.record.sort_ts || 0); });
    rows = runHooks('beforeRender', rows);
    state.rows = rows;
    ui.summary.textContent = rows.length + ' file(s) | browse mode | total: ' + state.browseRecords.length;
    renderList(rows);
    runHooks('afterRender', rows);
  };

  let searchAbort = null;
  const runSearch = () => {
    state.mode = 'search';
    const q = state.query;
    if (!q) { applyBrowseFilters(); return; }
    if (searchAbort) searchAbort.abort();
    searchAbort = new AbortController();
    ui.summary.textContent = 'Searching\u2026';
    const params = new URLSearchParams({ q: q, limit: '100' });
    if (state.project) params.set('project', state.project);
    if (state.scoreMin > 0) params.set('score', String(state.scoreMin));
    fetch('/api/search/semantic?' + params.toString(), { signal: searchAbort.signal })
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (!data.ok) { ui.summary.textContent = 'Search error: ' + (data.error || 'unknown'); return; }
        let rows = data.results.map(function(r) { return { record: r, score: r.score || 0 }; });
        rows = rows.filter(function(r) {
          if (state.agent && r.record.agent !== state.agent) return false;
          if (state.kind && r.record.kind !== state.kind) return false;
          return true;
        });
        if (state.sort === 'score') rows.sort(function(a, b) { return b.score - a.score; });
        else if (state.sort === 'oldest') rows.sort(function(a, b) { return (a.record.sort_ts || a.record.date || '') < (b.record.sort_ts || b.record.date || '') ? -1 : 1; });
        rows = runHooks('beforeRender', rows);
        state.rows = rows;
        ui.summary.textContent = rows.length + ' result(s) | fuzzy search | scanned: ' + (data.total_scanned || '?');
        renderList(rows);
        runHooks('afterRender', rows);
      })
      .catch(function(err) { if (err.name === 'AbortError') return; ui.summary.textContent = 'Search failed: ' + err.message; });
  };

  const refresh = () => {
    state.query = (ui.search.value || '').trim().toLowerCase();
    state.project = ui.project.value;
    state.agent = ui.agent.value;
    state.kind = ui.kind.value;
    state.sort = ui.sort.value;
    state.scoreMin = parseInt(ui.score.value, 10) || 0;
    pushUrlState();
    if (state.query) { runSearch(); } else { loadBrowseData(); }
  };

  /* --- event wiring ------------------------------------------------------ */
  const DEBOUNCE_MS = 800;
  let debounceTimer = null;
  const liveCheckbox = $('ctx-live');
  const scheduleRefresh = () => { clearTimeout(debounceTimer); debounceTimer = setTimeout(refresh, DEBOUNCE_MS); };
  ui.search.addEventListener('input', function() { if (liveCheckbox.checked) scheduleRefresh(); });
  ui.search.addEventListener('keydown', function(e) { if (e.key === 'Enter') { clearTimeout(debounceTimer); refresh(); } });
  ['input', 'change'].forEach(function(ev) {
    ui.project.addEventListener(ev, refresh);
    ui.agent.addEventListener(ev, refresh);
    ui.kind.addEventListener(ev, refresh);
    ui.sort.addEventListener(ev, refresh);
  });
  liveCheckbox.addEventListener('change', function() { if (liveCheckbox.checked) scheduleRefresh(); });
  ui.score.addEventListener('input', function() { ui.scoreLabel.textContent = ui.score.value; });
  ui.score.addEventListener('change', refresh);
  if (ui.expand) ui.expand.addEventListener('click', expandDetail);
  ui.copyPath.addEventListener('click', async function() {
    const p = state.selectedRecord?.absolute_path || state.selectedRecord?.path || state.selectedRecord?.relative_path || '';
    if (p && navigator.clipboard) { try { await navigator.clipboard.writeText(p); } catch (_) {} }
  });

  /* --- time buttons ------------------------------------------------------ */
  const setTimeBtnActive = (since) => {
    document.querySelectorAll('.time-btn').forEach(function(btn) {
      btn.classList.toggle('active', btn.dataset.since === since);
    });
  };
  document.querySelectorAll('.time-btn').forEach(function(btn) {
    btn.addEventListener('click', function() {
      state.since = btn.dataset.since;
      setTimeBtnActive(state.since);
      refresh();
    });
  });

  /* --- regenerate -------------------------------------------------------- */
  if (ui.regenerateBtn) {
    ui.regenerateBtn.addEventListener('click', function() {
      ui.regenerateBtn.disabled = true; ui.regenerateBtn.textContent = '\u2026';
      fetch('/api/regenerate', { method: 'POST', headers: { 'x-ai-contexters-action': 'regenerate' } })
        .then(function(r) { return r.json(); })
        .then(function(data) { if (data.ok) loadBrowseData(); else alert('Regenerate failed: ' + (data.error || 'unknown')); })
        .catch(function(err) { alert('Regenerate error: ' + err.message); })
        .finally(function() { ui.regenerateBtn.disabled = false; ui.regenerateBtn.textContent = '\u21BB'; });
    });
  }

  /* --- resizable panels -------------------------------------------------- */
  const resizeHandle = $('ctx-resize-handle');
  const layoutEl = $('ctx-layout');
  if (resizeHandle && layoutEl) {
    const SK = 'aicx-split-ratio';
    const saved = localStorage.getItem(SK);
    if (saved) { const r = parseFloat(saved); if (r > 0 && r < 1) layoutEl.style.gridTemplateColumns = r + 'fr 6px ' + (1 - r) + 'fr'; }
    let dragging = false;
    resizeHandle.addEventListener('mousedown', function(e) { e.preventDefault(); dragging = true; resizeHandle.classList.add('dragging'); document.body.style.cursor = 'col-resize'; document.body.style.userSelect = 'none'; });
    document.addEventListener('mousemove', function(e) { if (!dragging) return; const rect = layoutEl.getBoundingClientRect(); const x = e.clientX - rect.left; const total = rect.width - 6; const lw = Math.max(250, Math.min(x, total - 300)); const ratio = lw / total; layoutEl.style.gridTemplateColumns = ratio + 'fr 6px ' + (1 - ratio) + 'fr'; localStorage.setItem(SK, ratio.toFixed(4)); });
    document.addEventListener('mouseup', function() { if (!dragging) return; dragging = false; resizeHandle.classList.remove('dragging'); document.body.style.cursor = ''; document.body.style.userSelect = ''; });
  }

  /* --- load browse data -------------------------------------------------- */
  const loadBrowseData = () => {
    ui.summary.textContent = 'Loading\u2026';
    const params = new URLSearchParams();
    if (state.project) params.set('project', state.project);
    if (state.agent) params.set('agent', state.agent);
    if (state.kind) params.set('kind', state.kind);
    if (state.sort) params.set('sort', state.sort);
    if (state.since) params.set('since', state.since);
    const qs = params.toString();
    fetch('/api/browse' + (qs ? '?' + qs : ''))
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (!data.ok) { ui.summary.textContent = 'Failed: ' + (data.error || 'unknown'); return; }
        state.browseRecords = data.records || [];
        fillSelect(ui.project, data.projects || []);
        fillSelect(ui.agent, data.agents || []);
        fillSelect(ui.kind, data.kinds || []);
        const s = data.stats || {};
        ui.statFiles.textContent = s.total_files || 0;
        ui.statProjects.textContent = s.total_projects || 0;
        ui.statDays.textContent = s.total_days || 0;
        ui.genInfo.textContent = 'Generated ' + (data.generated_at || '?');
        ui.assumptions.innerHTML = '';
        (data.assumptions || []).forEach(function(a) { const li = document.createElement('li'); li.textContent = a; ui.assumptions.appendChild(li); });
        applyBrowseFilters();
      })
      .catch(function(err) { ui.summary.textContent = 'Load failed: ' + err.message; });
  };

  window.AIContextersDashboard = {
    version: '6.0.0-pwa',
    state: state,
    registerHook: function(name, fn) { if (!hooks[name] || typeof fn !== 'function') return false; hooks[name].push(fn); return true; },
    refresh: refresh,
    reload: loadBrowseData,
  };

  readUrlState();
  loadBrowseData();
})();
"#;
