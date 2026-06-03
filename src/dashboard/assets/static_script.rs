//! Static dashboard browser JavaScript.

pub(crate) const DASHBOARD_SCRIPT: &str = r#"
(() => {
  const dataNode = document.getElementById('ctx-data');
  if (!dataNode) return;

  let payload = null;
  try {
    payload = JSON.parse(dataNode.textContent || '{}');
  } catch (_err) {
    return;
  }

  const records = Array.isArray(payload.records) ? payload.records : [];
  const ui = {
    search: document.getElementById('ctx-search'),
    project: document.getElementById('ctx-project'),
    agent: document.getElementById('ctx-agent'),
    kind: document.getElementById('ctx-kind'),
    summary: document.getElementById('ctx-summary'),
    list: document.getElementById('ctx-list'),
    detailTitle: document.getElementById('ctx-detail-title'),
    detailMeta: document.getElementById('ctx-detail-meta'),
    detailPath: document.getElementById('ctx-detail-path'),
    detailPreview: document.getElementById('ctx-detail-preview'),
    detailContent: document.getElementById('ctx-detail-content'),
    assumptions: document.getElementById('ctx-assumptions'),
    copyPath: document.getElementById('ctx-copy-path'),
  };

  if (!ui.search || !ui.project || !ui.agent || !ui.kind || !ui.summary || !ui.list || !ui.detailTitle || !ui.detailMeta || !ui.detailPath || !ui.detailPreview || !ui.detailContent || !ui.assumptions || !ui.copyPath) {
    return;
  }

  const hooks = {
    beforeRender: [],
    afterRender: [],
    onSelect: [],
  };

  const state = {
    query: '',
    queryRaw: '',
    project: '',
    agent: '',
    kind: '',
    limit: 350,
    selectedId: null,
    rows: [],
    selectedRecord: null,
  };

  const normalizeText = (text) => {
    const map = {
      '\u0104':'A','\u0105':'a','\u0106':'C','\u0107':'c',
      '\u0118':'E','\u0119':'e','\u0141':'L','\u0142':'l',
      '\u0143':'N','\u0144':'n','\u00D3':'O','\u00F3':'o',
      '\u015A':'S','\u015B':'s','\u0179':'Z','\u017A':'z',
      '\u017B':'Z','\u017C':'z'
    };
    return (text || '')
      .toString()
      .replace(/[\u0104\u0105\u0106\u0107\u0118\u0119\u0141\u0142\u0143\u0144\u00D3\u00F3\u015A\u015B\u0179\u017A\u017B\u017C]/g,
        function(c) { return map[c] || c; })
      .toLowerCase();
  };

  const normalize = (value) =>
    normalizeText(value)
      .normalize('NFKD')
      .replace(/[\u0300-\u036f]/g, '')
      .replace(/\s+/g, ' ')
      .trim();

  const fillSelect = (node, values) => {
    values.forEach((value) => {
      const option = document.createElement('option');
      option.value = value;
      option.textContent = value;
      node.appendChild(option);
    });
  };

  fillSelect(ui.project, Array.isArray(payload.projects) ? payload.projects : []);
  fillSelect(ui.agent, Array.isArray(payload.agents) ? payload.agents : []);
  fillSelect(ui.kind, Array.isArray(payload.kinds) ? payload.kinds : []);

  (Array.isArray(payload.assumptions) ? payload.assumptions : []).forEach((item) => {
    const li = document.createElement('li');
    li.textContent = item;
    ui.assumptions.appendChild(li);
  });

  const uniqueChars = (text) => {
    const set = new Set();
    for (const ch of text) set.add(ch);
    return set;
  };

  const charJaccard = (a, b) => {
    if (!a || !b) return 0;
    const sa = uniqueChars(a);
    const sb = uniqueChars(b);
    let inter = 0;
    for (const ch of sa) {
      if (sb.has(ch)) inter += 1;
    }
    const union = sa.size + sb.size - inter;
    return union > 0 ? inter / union : 0;
  };

  const subsequenceScore = (needle, haystack) => {
    if (!needle || !haystack) return 0;
    let i = 0;
    let j = 0;
    while (i < needle.length && j < haystack.length) {
      if (needle[i] === haystack[j]) i += 1;
      j += 1;
    }
    return i / needle.length;
  };

  const tokenScore = (token, field, weight) => {
    if (!token || !field) return 0;

    if (field.includes(token)) {
      return weight * (1 + Math.min(token.length / 12, 1));
    }

    const subseq = subsequenceScore(token, field);
    if (subseq < 0.7) return 0;

    const jac = charJaccard(token, field);
    return weight * (0.35 * subseq + 0.15 * jac);
  };

  const fieldsForRecord = (record) => ({
    project: normalize(record.project),
    agent: normalize(record.agent),
    fileName: normalize(record.file_name),
    relPath: normalize(record.relative_path),
    preview: normalize(record.preview),
    blob: normalize(record.search_blob),
  });

  const scoreRecord = (record, tokens) => {
    if (!tokens.length) return 1;

    const fields = fieldsForRecord(record);
    let total = 0;

    for (const token of tokens) {
      const best = Math.max(
        tokenScore(token, fields.project, 2.3),
        tokenScore(token, fields.agent, 2.0),
        tokenScore(token, fields.fileName, 1.9),
        tokenScore(token, fields.relPath, 1.7),
        tokenScore(token, fields.preview, 1.2),
        tokenScore(token, fields.blob, 1.0),
      );
      total += best;
    }

    const threshold = Math.max(0.22 * tokens.length, 0.35);
    return total >= threshold ? total : 0;
  };

  const runHooks = (name, value) => {
    const list = hooks[name] || [];
    return list.reduce((acc, fn) => {
      try {
        const maybe = fn(acc, payload, state);
        return maybe === undefined ? acc : maybe;
      } catch (_err) {
        return acc;
      }
    }, value);
  };

  const escapeHtml = (text) => {
    const div = document.createElement('div');
    div.appendChild(document.createTextNode(text));
    return div.innerHTML;
  };

  const escapeRegex = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

  const highlightQuery = () => state.queryRaw || state.query;

  const highlightTerms = (text, query) => {
    if (!query || !text) return escapeHtml(text || '');
    const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return escapeHtml(text);

    const kinds = new Array(text.length).fill('');
    const markRange = (start, len, cls, overwrite) => {
      const end = Math.min(text.length, start + len);
      for (let i = start; i < end; i += 1) {
        if (overwrite || !kinds[i]) kinds[i] = cls;
      }
    };

    terms.forEach((term) => {
      const re = new RegExp(escapeRegex(term), 'gi');
      let match;
      while ((match = re.exec(text)) !== null) {
        if (!match[0]) break;
        markRange(match.index, match[0].length, 'hl', true);
      }
    });

    const normalizedText = normalizeText(text);
    terms.map(normalizeText).filter(Boolean).forEach((term) => {
      let searchFrom = 0;
      while (searchFrom < normalizedText.length) {
        const idx = normalizedText.indexOf(term, searchFrom);
        if (idx === -1) break;
        markRange(idx, term.length, 'hl-fuzzy', false);
        searchFrom = idx + Math.max(term.length, 1);
      }
    });

    let html = '';
    let start = 0;
    while (start < text.length) {
      const cls = kinds[start];
      let end = start + 1;
      while (end < text.length && kinds[end] === cls) end += 1;
      const chunk = escapeHtml(text.slice(start, end));
      html += cls ? '<mark class="' + cls + '">' + chunk + '</mark>' : chunk;
      start = end;
    }
    return html;
  };

  const renderDetail = (record, score) => {
    state.selectedRecord = record || null;

    if (!record) {
      ui.detailTitle.textContent = 'No result selected';
      ui.detailMeta.textContent = '';
      ui.detailPath.textContent = '';
      ui.detailPreview.textContent = '';
      ui.detailContent.textContent = 'Use search or filters to pick a note.';
      return;
    }

    const detailTitle = record.file_name || '(unnamed file)';
    const detailMeta = `${record.project || 'unknown'} | ${record.agent || 'unknown'} | ${record.kind || 'unknown'} | score ${Math.round(Number(score || 0) * 100)}/100`;
    const detailPath = record.absolute_path || record.relative_path || '';
    ui.detailTitle.innerHTML = highlightTerms(detailTitle, highlightQuery());
    ui.detailMeta.innerHTML = highlightTerms(detailMeta, highlightQuery());
    ui.detailPath.innerHTML = highlightTerms(detailPath, highlightQuery());
    ui.detailPreview.innerHTML = highlightTerms(record.preview || '', highlightQuery());
    ui.detailContent.innerHTML =
      highlightTerms(record.detail_text || record.preview || '(no content)', highlightQuery());
  };

  const mkBadge = (txt) => {
    const node = document.createElement('span');
    node.className = 'badge';
    node.innerHTML = highlightTerms(String(txt || ''), highlightQuery());
    return node;
  };

  const renderList = (rows) => {
    ui.list.innerHTML = '';

    if (!rows.length) {
      const empty = document.createElement('div');
      empty.className = 'empty';
      empty.textContent = 'No records match current query/filters.';
      ui.list.appendChild(empty);
      renderDetail(null, 0);
      return;
    }

    const visible = rows.slice(0, state.limit);

    if (!state.selectedId || !visible.some((r) => r.record.id === state.selectedId)) {
      state.selectedId = visible[0].record.id;
    }

    visible.forEach(({ record, score }) => {
      const item = document.createElement('button');
      item.type = 'button';
      item.className = 'result-item' + (record.id === state.selectedId ? ' active' : '');

      const top = document.createElement('div');
      top.className = 'result-top';

      top.appendChild(mkBadge(record.project || 'project'));
      top.appendChild(mkBadge(record.agent || 'agent'));
      top.appendChild(mkBadge(record.kind || 'kind'));
      top.appendChild(mkBadge(record.date || 'date'));
      top.appendChild(mkBadge(`${Math.round(Number(score) * 100)}/100`));

      const name = document.createElement('div');
      name.className = 'result-name';
      const nameText = `${record.file_name || '(unnamed)'} • ${record.size_human || ''}`;
      name.innerHTML = highlightTerms(nameText, highlightQuery());

      item.appendChild(top);
      item.appendChild(name);
      if (state.query && record.preview) {
        const preview = document.createElement('div');
        preview.className = 'result-preview';
        preview.innerHTML = highlightTerms(record.preview, highlightQuery());
        item.appendChild(preview);
      }

      item.addEventListener('click', () => {
        state.selectedId = record.id;
        renderList(state.rows);
        renderDetail(record, score);
        runHooks('onSelect', record);
      });

      ui.list.appendChild(item);
    });

    const selected = visible.find((r) => r.record.id === state.selectedId) || visible[0];
    if (selected) {
      renderDetail(selected.record, selected.score);
    }
  };

  const refresh = () => {
    state.queryRaw = ui.search.value || '';
    state.query = normalize(ui.search.value);
    state.project = ui.project.value;
    state.agent = ui.agent.value;
    state.kind = ui.kind.value;

    const tokens = state.query.split(' ').filter(Boolean);

    let rows = records
      .filter((record) => {
        if (state.project && record.project !== state.project) return false;
        if (state.agent && record.agent !== state.agent) return false;
        if (state.kind && record.kind !== state.kind) return false;
        return true;
      })
      .map((record) => ({
        record,
        score: scoreRecord(record, tokens),
      }))
      .filter((row) => row.score > 0)
      .sort((a, b) => {
        if (b.score !== a.score) return b.score - a.score;
        return (b.record.sort_ts || 0) - (a.record.sort_ts || 0);
      });

    rows = runHooks('beforeRender', rows);
    state.rows = rows;

    ui.summary.textContent = `${rows.length} fuzzy match(es) | showing up to ${state.limit} | total files: ${records.length}`;

    renderList(rows);
    runHooks('afterRender', rows);
  };

  /* --- debounced search ------------------------------------------------- */
  const DEBOUNCE_MS = 800;
  let debounceTimer = null;
  const liveCheckbox = document.getElementById('ctx-live');

  const scheduleRefresh = () => {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(refresh, DEBOUNCE_MS);
  };

  ui.search.addEventListener('input', () => {
    if (liveCheckbox.checked) {
      scheduleRefresh();              // live mode: 800 ms debounce
    }
    // non-live: wait for Enter or space (handled below)
  });

  ui.search.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      clearTimeout(debounceTimer);
      refresh();
    }
  });

  // dropdowns always refresh immediately
  ['input', 'change'].forEach((eventName) => {
    ui.project.addEventListener(eventName, refresh);
    ui.agent.addEventListener(eventName, refresh);
    ui.kind.addEventListener(eventName, refresh);
  });

  liveCheckbox.addEventListener('change', () => {
    if (liveCheckbox.checked) scheduleRefresh();
  });

  ui.copyPath.addEventListener('click', async () => {
    const path = state.selectedRecord?.absolute_path || state.selectedRecord?.relative_path || '';
    if (!path || !navigator.clipboard) return;
    try {
      await navigator.clipboard.writeText(path);
    } catch (_err) {
      // no-op
    }
  });

  /* --- resizable panels ------------------------------------------------ */
  const resizeHandle = document.getElementById('ctx-resize-handle');
  const layoutEl = document.getElementById('ctx-layout');

  if (resizeHandle && layoutEl) {
    const STORAGE_KEY = 'aicx-split-ratio';
    const MIN_LIST = 250;
    const MIN_DETAIL = 300;

    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved) {
      const ratio = parseFloat(saved);
      if (ratio > 0 && ratio < 1) {
        layoutEl.style.gridTemplateColumns = `${ratio}fr 6px ${1 - ratio}fr`;
      }
    }

    let dragging = false;

    resizeHandle.addEventListener('mousedown', (e) => {
      e.preventDefault();
      dragging = true;
      resizeHandle.classList.add('dragging');
      document.body.style.cursor = 'col-resize';
      document.body.style.userSelect = 'none';
    });

    document.addEventListener('mousemove', (e) => {
      if (!dragging) return;
      const rect = layoutEl.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const total = rect.width - 6;
      const listW = Math.max(MIN_LIST, Math.min(x, total - MIN_DETAIL));
      const ratio = listW / total;
      layoutEl.style.gridTemplateColumns = `${ratio}fr 6px ${1 - ratio}fr`;
      localStorage.setItem(STORAGE_KEY, ratio.toFixed(4));
    });

    document.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      resizeHandle.classList.remove('dragging');
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    });
  }

  window.AIContextersDashboard = {
    version: '4.0.0',
    payload,
    state,
    registerHook(name, fn) {
      if (!hooks[name] || typeof fn !== 'function') return false;
      hooks[name].push(fn);
      return true;
    },
    refresh,
  };

  refresh();
})();
"#;
