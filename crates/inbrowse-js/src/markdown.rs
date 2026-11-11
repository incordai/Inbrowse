//! Shared markdown extraction script used by the LP.getMarkdown CDP method
//! and the CLI `--dump markdown` mode. Lives in inbrowse so both
//! inbrowse-cdp and inbrowse-cli can call it without depending on each other.

/// The converter as a JS function of one argument, the root element (falsy
/// for `document.body`). A macro so both public forms at the end of this file
/// share one copy of the script.
macro_rules! markdown_function {
    () => {
        r#"
(function(root) {
    // GFM table: header row, `|---|` separator, then one line per row with
    // no blank lines between. Cells collapse to a single line with `|`
    // escaped; colspan repeats empty cells and short rows are padded. The
    // caption becomes a paragraph above or below per its caption-side.
    function tableToMd(table, depth) {
        var rows = [];
        var header = null;
        function collect(parent, inHead) {
            var kids = parent.childNodes || [];
            for (var i = 0; i < kids.length; i++) {
                var k = kids[i];
                if (k.nodeType !== 1) continue;
                var t = k.tagName.toLowerCase();
                if (t === 'thead') collect(k, true);
                else if (t === 'tbody' || t === 'tfoot') collect(k, false);
                else if (t === 'tr') {
                    var cells = [];
                    var allTh = true;
                    var cs = k.childNodes || [];
                    for (var j = 0; j < cs.length; j++) {
                        var c = cs[j];
                        if (c.nodeType !== 1) continue;
                        var ct = c.tagName.toLowerCase();
                        if (ct !== 'td' && ct !== 'th') continue;
                        if (ct !== 'th') allTh = false;
                        var text = toMd(c, depth).replace(/\s*\n+\s*/g, ' ').trim().replace(/\|/g, '\\|');
                        cells.push(text);
                        var span = parseInt(c.getAttribute('colspan') || '1', 10);
                        for (var s2 = 1; s2 < span && s2 < 1000; s2++) cells.push('');
                    }
                    if (!cells.length) continue;
                    if (header === null && rows.length === 0 && (inHead || allTh)) header = cells;
                    else rows.push(cells);
                }
            }
        }
        collect(table, false);
        if (header === null) {
            if (!rows.length) return '';
            header = rows.shift();
        }
        var width = header.length;
        for (var r = 0; r < rows.length; r++) width = Math.max(width, rows[r].length);
        function line(cells) {
            var out = [];
            for (var i = 0; i < width; i++) out.push(cells[i] || '');
            return '| ' + out.join(' | ') + ' |';
        }
        var sep = [];
        for (var w = 0; w < width; w++) sep.push('---');
        var md = [line(header), '|' + sep.join('|') + '|'];
        for (var r2 = 0; r2 < rows.length; r2++) md.push(line(rows[r2]));
        var body = md.join('\n');
        var caption = null;
        var tk = table.childNodes || [];
        for (var q = 0; q < tk.length; q++) {
            if (tk[q].nodeType === 1 && tk[q].tagName.toLowerCase() === 'caption') { caption = tk[q]; break; }
        }
        if (caption) {
            var capKids = caption.childNodes || [];
            var capRaw = '';
            for (var ck = 0; ck < capKids.length; ck++) capRaw += toMd(capKids[ck], depth);
            var capText = capRaw.replace(/\s*\n+\s*/g, ' ').trim();
            var side = '';
            try { side = getComputedStyle(caption).captionSide || ''; } catch (e) {}
            if (!side) side = (caption.getAttribute('style') || '').match(/caption-side\s*:\s*bottom/i) ? 'bottom' : 'top';
            if (capText) body = side === 'bottom' ? body + '\n\n' + capText : capText + '\n\n' + body;
        }
        return '\n\n' + body + '\n\n';
    }
    function toMd(el, depth) {
        if (!el) return '';
        // Source whitespace (indentation, line wraps) collapses to one space;
        // only block structure below produces line breaks.
        if (el.nodeType === 3) return (el.textContent || '').replace(/[\t\n\f\r ]+/g, ' ');
        if (el.nodeType !== 1) return '';
        var tag = (el.tagName || '').toLowerCase();
        if (el.hasAttribute && el.hasAttribute('hidden')) return '';
        switch (tag) {
            case 'script': case 'style': case 'noscript': case 'link': case 'meta':
            case 'template': case 'option': case 'optgroup':
                return '';
            case 'pre':
                return '\n```\n' + (el.textContent || '').replace(/\n$/, '') + '\n```\n\n';
            case 'select':
                return selectedOptionText(el);
            case 'ul': case 'ol':
                return listToMd(el, depth);
            case 'table':
                return tableToMd(el, depth);
        }
        var children = '';
        var cn = el.childNodes || [];
        for (var i = 0; i < cn.length; i++) children += toMd(cn[i], depth);
        children = children.replace(/\n{3,}/g, '\n\n');
        switch(tag) {
            case 'h1': return '\n# ' + children.trim() + '\n\n';
            case 'h2': return '\n## ' + children.trim() + '\n\n';
            case 'h3': return '\n### ' + children.trim() + '\n\n';
            case 'h4': return '\n#### ' + children.trim() + '\n\n';
            case 'h5': return '\n##### ' + children.trim() + '\n\n';
            case 'h6': return '\n###### ' + children.trim() + '\n\n';
            case 'p': return '\n' + children.trim() + '\n\n';
            case 'br': return '\n';
            case 'hr': return '\n---\n\n';
            case 'strong': case 'b': return '**' + children + '**';
            case 'em': case 'i': return '*' + children + '*';
            case 'code': return '`' + children + '`';
            case 'blockquote': return '\n> ' + children.trim().replace(/\n/g, '\n> ') + '\n\n';
            case 'a':
                var href = el.getAttribute('href') || '';
                // Block children inside a link (cards, two-line labels) are
                // joined with a space; link text is one line.
                var label = children.replace(/\s*\n+\s*/g, ' ').trim();
                if (href && label) return '[' + label + '](' + href + ')';
                return children;
            case 'img':
                var src = el.getAttribute('src') || '';
                var alt = el.getAttribute('alt') || '';
                return '![' + alt + '](' + src + ')';
            case 'caption': return '';
            case 'thead': case 'tbody': case 'tfoot': case 'tr': case 'th': case 'td': return children;
            case 'div': case 'section': case 'article': case 'main': case 'aside': case 'nav': case 'header':
            case 'footer': case 'li': case 'dl': case 'dt': case 'dd': case 'figure': case 'figcaption':
            case 'form': case 'fieldset': case 'details': case 'summary': case 'address':
                return '\n' + children + '\n';
            case 'span': return children;
            default: return children;
        }
    }
    // A list item per line: `- ` or the item number, with every further line
    // of the item (wrapped text, a nested list) indented under its marker so
    // nesting survives. Ordered lists honour `start` and an item's `value`.
    function listToMd(list, depth) {
        var ordered = list.tagName.toLowerCase() === 'ol';
        var number = parseInt(list.getAttribute('start') || '1', 10);
        if (!isFinite(number)) number = 1;
        var lines = [];
        var kids = list.childNodes || [];
        for (var i = 0; i < kids.length; i++) {
            var item = kids[i];
            if (item.nodeType !== 1 || item.tagName.toLowerCase() !== 'li') continue;
            if (item.hasAttribute('hidden')) continue;
            if (ordered && item.hasAttribute('value')) {
                var value = parseInt(item.getAttribute('value'), 10);
                if (isFinite(value)) number = value;
            }
            var marker = ordered ? (number++) + '. ' : '- ';
            var content = '';
            var parts = item.childNodes || [];
            for (var j = 0; j < parts.length; j++) content += toMd(parts[j], depth + 1);
            var itemLines = content.replace(/\n{3,}/g, '\n\n').trim().split('\n');
            var indent = new Array(marker.length + 1).join(' ');
            lines.push(marker + itemLines[0].trim());
            for (var k = 1; k < itemLines.length; k++) {
                lines.push(itemLines[k].trim() ? indent + itemLines[k] : '');
            }
        }
        return '\n' + lines.join('\n') + '\n\n';
    }
    // innerText of a <select>: its selected option only (the last with the
    // `selected` state, else the first enabled one).
    function selectedOptionText(select) {
        var options = select.querySelectorAll('option');
        var chosen = null;
        for (var i = 0; i < options.length; i++) if (options[i].selected) chosen = options[i];
        for (var j = 0; !chosen && j < options.length; j++) if (!options[j].disabled) chosen = options[j];
        return chosen ? (chosen.textContent || '').replace(/[\t\n\f\r ]+/g, ' ').trim() : '';
    }
    // Lines outside code fences lose trailing spaces, whitespace-only lines
    // become empty, and blank runs collapse to one blank line.
    function tidy(md) {
        var pieces = md.split(/(\n```\n[\s\S]*?\n```\n)/);
        for (var i = 0; i < pieces.length; i += 2) {
            pieces[i] = pieces[i].replace(/[ \t]+$/gm, '').replace(/^[ \t]+$/gm, '');
        }
        return pieces.join('').replace(/\n{3,}/g, '\n\n').trim();
    }
    var body = root || document.body || document.documentElement;
    return tidy(toMd(body, 0));
})"#
    };
}

/// JS expression that walks `document.body` and returns a markdown string.
/// Must be evaluated against a Page that has a fully-bootstrapped JS runtime.
pub const HTML_TO_MARKDOWN_JS: &str = concat!(markdown_function!(), "(null)\n");

/// [`HTML_TO_MARKDOWN_JS`] for the subtree of the first element matching
/// `selector` (`fetch --selector`). Evaluates to `null` when nothing matches.
pub fn html_to_markdown_js_within(selector: &str) -> String {
    let selector = serde_json::Value::from(selector).to_string();
    format!(
        "(function(){{var root=document.querySelector({selector});return root?{}(root):null;}})()",
        markdown_function!()
    )
}
