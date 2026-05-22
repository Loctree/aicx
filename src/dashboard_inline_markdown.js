const AicxMarkdown = (() => {
  const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

  const inlineMarkdown = (s) => {
    let html = esc(s)
      .replace(/`([^`]+)`/g, '<code>$1</code>')
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(/\*([^*]+)\*/g, '<em>$1</em>');
    return html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (match, label, url) => {
      const u = url.trim();
      const lower = u.toLowerCase();
      if (lower.startsWith('javascript:') || lower.startsWith('data:') || lower.startsWith('vbscript:') || lower.startsWith('blob:') || lower.startsWith('file:')) {
        return '[' + label + '](' + url + ')';
      }
      const href = u.replace(/"/g, '&quot;');
      return '<a href="' + href + '" target="_blank" rel="noopener noreferrer">' + label + '</a>';
    });
  };

  const renderMarkdown = (src) => {
    if (!src) return '';
    let html = '';
    let inCode = false;
    let codeLang = '';
    let codeLines = [];
    const lines = src.split('\n');
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (inCode) {
        if (line.startsWith('```')) {
          html += '<pre><code' + (codeLang ? ' class="lang-' + esc(codeLang) + '"' : '') + '>' + esc(codeLines.join('\n')) + '</code></pre>';
          inCode = false; codeLines = []; codeLang = '';
        } else { codeLines.push(line); }
        continue;
      }
      if (line.startsWith('```')) { inCode = true; codeLang = line.slice(3).trim(); continue; }
      if (line.startsWith('---') && line.replace(/-/g, '').trim() === '') { html += '<hr>'; continue; }
      const hm = line.match(/^(#{1,6})\s+(.*)/);
      if (hm) { const lvl = hm[1].length; html += '<h' + lvl + '>' + inlineMarkdown(hm[2]) + '</h' + lvl + '>'; continue; }
      if (line.startsWith('> ')) { html += '<blockquote>' + inlineMarkdown(line.slice(2)) + '</blockquote>'; continue; }
      const lm = line.match(/^(\s*[-*])\s+(.*)/);
      if (lm) { html += '<ul><li>' + inlineMarkdown(lm[2]) + '</li></ul>'; continue; }
      const om = line.match(/^(\s*\d+\.)\s+(.*)/);
      if (om) { html += '<ol><li>' + inlineMarkdown(om[2]) + '</li></ol>'; continue; }
      if (line.trim() === '') { html += '<br>'; continue; }
      html += '<p>' + inlineMarkdown(line) + '</p>';
    }
    if (inCode) { html += '<pre><code>' + esc(codeLines.join('\n')) + '</code></pre>'; }
    return html.replace(/<\/ul><ul>/g, '').replace(/<\/ol><ol>/g, '');
  };

  return { inlineMarkdown, renderMarkdown };
})();

if (typeof globalThis !== 'undefined') {
  globalThis.AicxMarkdown = AicxMarkdown;
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = AicxMarkdown;
}
