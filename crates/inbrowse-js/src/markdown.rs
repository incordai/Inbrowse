//! Shared markdown extraction script used by the LP.getMarkdown CDP method,
//! the CLI `--dump markdown` and `--dump json` modes and the MCP
//! `browser_markdown` tool. Lives in inbrowse so inbrowse-cdp, inbrowse-cli and
//! inbrowse-mcp can call it without depending on each other.

/// The converter as a JS function of two arguments: the root element (falsy for
/// `document.body`) and an options object (`dropBoilerplate`). It returns
/// `{content, links, images}`: the markdown, and every link and image it
/// rendered as `[url, text]` pairs, each URL absolute (against `<base href>`
/// when the document has one). A macro so the public forms at the end of this
/// file share one copy of the script.
macro_rules! markdown_function {
    () => {
        r#"
(function(root, options) {
    var dropBoilerplate = !!(options && options.dropBoilerplate);
    var codeBlocks = [];
    var links = [];
    var images = [];
    // Open article/main and section elements around the node being rendered.
    // A header, footer or aside inside one belongs to that content.
    var inArticle = 0;
    var inSection = 0;
    var CODE = '\u0001';

    function oneLine(text) { return text.replace(/\s+/g, ' ').trim(); }
    function styleOf(el) {
        try { return getComputedStyle(el); } catch (e) { return null; }
    }
    function resolveUrl(raw, el) {
        raw = (raw || '').trim();
        if (!raw) return '';
        try { return new URL(raw, el.baseURI || document.baseURI).href; } catch (e) { return raw; }
    }
    // A data: image would put its whole payload in the output; keep its type.
    function imageUrl(raw, el) {
        var url = resolveUrl(raw, el);
        return /^data:/i.test(url) ? url.replace(/^(data:[^;,]*)[\s\S]*$/i, '$1') : url;
    }
    function linkUrl(el) {
        var raw = (el.getAttribute('href') || '').trim();
        if (!raw || raw === '#') return '';
        var url = resolveUrl(raw, el);
        return /^javascript:/i.test(url) ? '' : url;
    }
    // The text a link stands for: its own text, else the alt text of an image
    // in it, else its aria-label or title.
    function linkText(el) {
        var text = oneLine(el.textContent || '');
        if (text) return text;
        var img = el.querySelector('img[alt]');
        text = img ? oneLine(img.getAttribute('alt')) : '';
        return text || oneLine(el.getAttribute('aria-label') || el.getAttribute('title') || '');
    }
    function codeFence(text, min) {
        var longest = 0;
        var runs = text.match(/`+/g) || [];
        for (var i = 0; i < runs.length; i++) longest = Math.max(longest, runs[i].length);
        return new Array(Math.max(min, longest + 1) + 1).join('`');
    }
    function inlineCode(text) {
        text = oneLine(text);
        if (!text) return '';
        var fence = codeFence(text, 1);
        return /^`|`$/.test(text) ? fence + ' ' + text + ' ' + fence : fence + text + fence;
    }
    function codeLanguage(pre) {
        var code = pre.querySelector('code');
        var sources = [pre, code];
        for (var i = 0; i < sources.length; i++) {
            var node = sources[i];
            if (!node) continue;
            var match = /(?:^|\s)(?:language|lang|highlight-source)-([\w+#.-]+)/.exec(node.getAttribute('class') || '');
            if (match) return match[1];
            var data = node.getAttribute('data-language') || node.getAttribute('data-lang');
            if (data && /^[\w+#.-]+$/.test(data)) return data;
        }
        return '';
    }
    // A <pre> loses its leading blank lines and trailing whitespace; the rest
    // stays verbatim. The block is rendered once the whole document is done
    // (see expandCode), so list and quote prefixes never reach its lines.
    function preToMd(pre, depth) {
        var text = (pre.textContent || '').replace(/^(?:[ \t]*\n)+/, '').replace(/\s+$/, '');
        if (!text) return '';
        codeBlocks.push({ lang: codeLanguage(pre), text: text });
        // Inside a list item the block continues the item without a blank line.
        return (depth ? '\n' : '\n\n') + CODE + (codeBlocks.length - 1) + CODE + '\n\n';
    }
    function expandCode(md) {
        var block = /^((?: |>|- |\d+\. )*)\u0001(\d+)\u0001$/;
        var lines = md.split('\n');
        var out = [];
        for (var i = 0; i < lines.length; i++) {
            var m = block.exec(lines[i]);
            if (m) {
                var code = codeBlocks[+m[2]];
                var fence = codeFence(code.text, 3);
                // Later lines sit under the first: list markers become spaces,
                // quote markers stay.
                var cont = m[1].replace(/[^> ]/g, ' ');
                var codeLines = code.text.split('\n');
                out.push(m[1] + fence + code.lang);
                for (var j = 0; j < codeLines.length; j++) {
                    out.push(codeLines[j] === '' ? cont.replace(/\s+$/, '') : cont + codeLines[j]);
                }
                out.push(cont + fence);
            } else {
                out.push(lines[i].replace(/\u0001(\d+)\u0001/g, function(whole, index) {
                    return inlineCode(codeBlocks[+index].text);
                }));
            }
        }
        return out.join('\n');
    }

    // Header and footer of the page, navigation and sidebars. Those inside an
    // article, main or section are part of it; a sidebar inside a section still
    // is not.
    function isBoilerplate(el, tag) {
        var kind = (el.getAttribute('role') || '').toLowerCase();
        if (kind === 'navigation') kind = 'nav';
        else if (kind === 'complementary') kind = 'aside';
        else if (kind === 'banner') kind = 'header';
        else if (kind === 'contentinfo') kind = 'footer';
        else kind = tag;
        if (kind === 'nav') return true;
        if (kind === 'aside') return inArticle === 0;
        if (kind === 'header' || kind === 'footer') return inArticle + inSection === 0;
        return false;
    }

    // GFM table: header row, `|---|` separator, then one line per row with
    // no blank lines between. Cells collapse to a single line with `|`
    // escaped; colspan repeats empty cells and short rows are padded. The
    // caption becomes a paragraph above or below per its caption-side.
    // The table is laid out first, without converting any cell: the plan holds
    // each row's cells. The frame then converts the cells in document order and
    // the caption last, and builds the table once they are all in.
    function tableFrame(table, depth) {
        var plan = [];
        var cellList = [];
        function collect(parent, inHead) {
            var kids = parent.childNodes || [];
            for (var i = 0; i < kids.length; i++) {
                var k = kids[i];
                if (k.nodeType !== 1) continue;
                var t = k.tagName.toLowerCase();
                if (isHidden(k)) continue;
                if (t === 'thead') collect(k, true);
                else if (t === 'tbody' || t === 'tfoot') collect(k, false);
                else if (t === 'tr') {
                    var row = { inHead: inHead, allTh: true, cells: [] };
                    var cs = k.childNodes || [];
                    for (var j = 0; j < cs.length; j++) {
                        var c = cs[j];
                        if (c.nodeType !== 1) continue;
                        var ct = c.tagName.toLowerCase();
                        if (ct !== 'td' && ct !== 'th') continue;
                        if (ct !== 'th') row.allTh = false;
                        var cell = { el: c, text: '', span: parseInt(c.getAttribute('colspan') || '1', 10) };
                        row.cells.push(cell);
                        cellList.push(cell);
                    }
                    if (row.cells.length) plan.push(row);
                }
            }
        }
        collect(table, false);
        var caption = null;
        var tk = table.childNodes || [];
        for (var q = 0; q < tk.length; q++) {
            if (tk[q].nodeType === 1 && tk[q].tagName.toLowerCase() === 'caption') { caption = tk[q]; break; }
        }
        var capKids = caption ? (caption.childNodes || []) : [];
        var capRaw = '';
        var cellIndex = 0;
        var capIndex = 0;
        var current = null;
        return {
            next: function() {
                if (cellIndex < cellList.length) {
                    current = cellList[cellIndex++];
                    return { el: current.el, depth: depth };
                }
                current = null;
                if (capIndex < capKids.length) return { el: capKids[capIndex++], depth: depth };
                return null;
            },
            take: function(md) {
                if (!current) { capRaw += md; return; }
                current.text = oneLine(md.replace(/\u0001(\d+)\u0001/g, function(whole, index) {
                    return inlineCode(codeBlocks[+index].text);
                })).replace(/\|/g, '\\|');
            },
            done: function() { return buildTable(plan, caption, capRaw); }
        };
    }
    function buildTable(plan, caption, capRaw) {
        var rows = [];
        var header = null;
        for (var p = 0; p < plan.length; p++) {
            var cells = [];
            for (var c = 0; c < plan[p].cells.length; c++) {
                cells.push(plan[p].cells[c].text);
                for (var s2 = 1; s2 < plan[p].cells[c].span && s2 < 1000; s2++) cells.push('');
            }
            if (header === null && rows.length === 0 && (plan[p].inHead || plan[p].allTh)) header = cells;
            else rows.push(cells);
        }
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
        if (caption) {
            var capText = capRaw.replace(/\s*\n+\s*/g, ' ').trim();
            var side = '';
            try { side = getComputedStyle(caption).captionSide || ''; } catch (e) {}
            if (!side) side = (caption.getAttribute('style') || '').match(/caption-side\s*:\s*bottom/i) ? 'bottom' : 'top';
            if (capText) body = side === 'bottom' ? body + '\n\n' + capText : capText + '\n\n' + body;
        }
        return '\n\n' + body + '\n\n';
    }
    // Hidden by the `hidden` attribute or by computed style (display: none).
    function isHidden(el) {
        if (el.hasAttribute('hidden')) return true;
        var style = styleOf(el);
        return !!style && style.display === 'none';
    }
    // Adjacent pieces share one space where both bring one, and a line does
    // not start with the space that followed the break before it.
    function join(text, piece) {
        var last = text.slice(-1);
        return (last === ' ' || last === '\n') && piece.charAt(0) === ' ' ? text + piece.slice(1) : text + piece;
    }
    // Emphasis delimiters hug the text: whitespace at either end moves outside
    // them, and an empty element leaves nothing.
    function wrap(content, delimiter) {
        var m = /^(\s*)([\s\S]*?)(\s*)$/.exec(content.replace(/\n{2,}/g, ' '));
        return m[2] ? m[1] + delimiter + m[2] + delimiter + m[3] : content;
    }
    // The tree is walked with an explicit stack of frames, never by recursion,
    // so a page nested thousands of levels deep cannot exhaust the call stack.
    // `enter` returns the markdown of a node that needs no children, or null
    // after pushing a frame. A frame asks for its children one at a time
    // (`next`, null when it has no more), receives each result (`take`) and
    // yields its own markdown (`done`).
    function enter(el, depth, stack) {
        if (!el) return '';
        // Source whitespace (indentation, line wraps) collapses to one space;
        // only block structure below produces line breaks.
        if (el.nodeType === 3) return (el.textContent || '').replace(/[\t\n\f\r ]+/g, ' ').replace(/\u0001/g, '');
        if (el.nodeType !== 1) return '';
        var tag = (el.tagName || '').toLowerCase();
        switch (tag) {
            case 'script': case 'style': case 'noscript': case 'link': case 'meta':
            case 'template': case 'option': case 'optgroup':
                return '';
        }
        var style = styleOf(el);
        if (el.hasAttribute('hidden') || (style && style.display === 'none')) return '';
        if (dropBoilerplate && el !== root && isBoilerplate(el, tag)) return '';
        var article = tag === 'article' || tag === 'main';
        var section = tag === 'section';
        if (article) inArticle++;
        if (section) inSection++;
        var md = open(el, tag, depth, style);
        if (typeof md === 'string') {
            if (article) inArticle--;
            if (section) inSection--;
            return md;
        }
        md.article = article;
        md.section = section;
        stack.push(md);
        return null;
    }
    function toMd(el, depth) {
        var stack = [];
        var md = enter(el, depth, stack);
        if (md !== null) return md;
        for (;;) {
            var frame = stack[stack.length - 1];
            var request = frame.next();
            if (request) {
                md = enter(request.el, request.depth, stack);
                if (md !== null) frame.take(md);
                continue;
            }
            stack.pop();
            md = frame.done();
            if (frame.article) inArticle--;
            if (frame.section) inSection--;
            if (!stack.length) return md;
            stack[stack.length - 1].take(md);
        }
    }
    // The markdown of an element that needs no children, else its frame.
    function open(el, tag, depth, style) {
        // visibility: hidden hides an element's own content, but a descendant
        // can set visibility: visible again.
        var ghost = !!style && (style.visibility === 'hidden' || style.visibility === 'collapse');
        switch (tag) {
            case 'pre': return ghost ? '' : preToMd(el, depth);
            case 'select': return ghost ? '' : selectedOptionText(el);
            case 'ul': case 'ol': return listFrame(el, depth);
            case 'table': return ghost ? '' : tableFrame(el, depth);
            case 'br': return ghost ? '' : '\n';
            case 'hr': return ghost ? '' : '\n\n---\n\n';
            case 'code': return ghost ? '' : inlineCode(el.textContent || '');
            case 'img':
                if (ghost) return '';
                var src = imageUrl(el.getAttribute('src'), el);
                if (!src) return '';
                var alt = oneLine(el.getAttribute('alt') || '');
                if (!/^data:/i.test(src)) images.push([src, alt]);
                return '![' + alt + '](' + src + ')';
        }
        var kids = el.childNodes || [];
        var next = 0;
        var children = '';
        return {
            next: function() {
                while (next < kids.length) {
                    var kid = kids[next++];
                    if (ghost && kid.nodeType !== 1) continue;
                    return { el: kid, depth: depth };
                }
                return null;
            },
            take: function(md) { children = join(children, md); },
            done: function() { return finishChildren(el, tag, children, ghost); }
        };
    }
    // An element's markdown once its children are rendered.
    function finishChildren(el, tag, children, ghost) {
        children = children.replace(/\n{3,}/g, '\n\n');
        if (ghost) return children.trim() ? '\n' + children + '\n' : '';
        var heading = /^h([1-6])$/.exec(tag);
        if (heading) {
            var title = oneLine(children);
            return title ? '\n\n' + new Array(+heading[1] + 1).join('#') + ' ' + title + '\n\n' : '';
        }
        switch (tag) {
            case 'p': return children.trim() ? '\n\n' + children.trim() + '\n\n' : '';
            case 'strong': case 'b': return wrap(children, '**');
            case 'em': case 'i': return wrap(children, '*');
            case 'del': case 's': case 'strike': return wrap(children, '~~');
            case 'blockquote':
                return children.trim() ? '\n\n> ' + children.trim().replace(/\n/g, '\n> ') + '\n\n' : '';
            case 'a':
                var href = linkUrl(el);
                if (!href) return children;
                links.push([href, linkText(el)]);
                // Block children inside a link (cards, two-line labels) are
                // joined with a space; link text is one line.
                var label = oneLine(children);
                return label ? '[' + label + '](' + href + ')' : children;
            case 'caption': return '';
            case 'div': case 'section': case 'article': case 'main': case 'aside': case 'nav': case 'header':
            case 'footer': case 'li': case 'dl': case 'dt': case 'dd': case 'figure': case 'figcaption':
            case 'form': case 'fieldset': case 'details': case 'summary': case 'address':
                return '\n' + children.replace(/^ +| +$/g, '') + '\n';
            default: return children;
        }
    }
    // A list item per line: `- ` or the item number, with every further line
    // of the item (wrapped text, a nested list) indented under its marker so
    // nesting survives. Ordered lists honour `start` and an item's `value`.
    function listFrame(list, depth) {
        var ordered = list.tagName.toLowerCase() === 'ol';
        var number = parseInt(list.getAttribute('start') || '1', 10);
        if (!isFinite(number)) number = 1;
        var lines = [];
        var kids = list.childNodes || [];
        var i = 0;
        // The item being rendered: its marker, its children and what they gave.
        var marker = '';
        var parts = null;
        var j = 0;
        var content = '';
        function finishItem() {
            var itemLines = content.replace(/\n{3,}/g, '\n\n').trim().split('\n');
            var indent = new Array(marker.length + 1).join(' ');
            lines.push(marker + itemLines[0].trim());
            for (var k = 1; k < itemLines.length; k++) {
                lines.push(itemLines[k].trim() ? indent + itemLines[k] : '');
            }
            parts = null;
        }
        return {
            next: function() {
                for (;;) {
                    if (parts) {
                        if (j < parts.length) return { el: parts[j++], depth: depth + 1 };
                        finishItem();
                    }
                    if (i >= kids.length) return null;
                    var item = kids[i++];
                    if (item.nodeType !== 1 || item.tagName.toLowerCase() !== 'li') continue;
                    if (isHidden(item)) continue;
                    if (ordered && item.hasAttribute('value')) {
                        var value = parseInt(item.getAttribute('value'), 10);
                        if (isFinite(value)) number = value;
                    }
                    marker = ordered ? (number++) + '. ' : '- ';
                    parts = item.childNodes || [];
                    j = 0;
                    content = '';
                }
            },
            take: function(md) { content = join(content, md); },
            // A list inside an item continues the item without a blank line.
            done: function() { return lines.length ? (depth ? '\n' : '\n\n') + lines.join('\n') + '\n\n' : ''; }
        };
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
    // Lines lose trailing spaces, whitespace-only lines become empty, and blank
    // runs collapse to one blank line. Code blocks are still placeholders here,
    // so they keep their own spacing.
    function tidy(md) {
        return md.replace(/[ \t]+$/gm, '').replace(/^[ \t]+$/gm, '').replace(/\n{3,}/g, '\n\n').trim();
    }
    var body = root || document.body || document.documentElement;
    if (root) {
        // A selected element inside an article is rendered as part of it.
        if (root.closest('article, main')) inArticle++;
        if (root.closest('section')) inSection++;
    }
    var md = toMd(body, 0);
    return { content: expandCode(tidy(md)), links: links, images: images };
})"#
    };
}

/// The script for `selector`'s first match, or `document.body` without one,
/// followed by `result`, an expression on the converter's return value `r`.
/// Evaluates to `null` when the selector matches nothing.
fn converter_script(selector: Option<&str>, drop_boilerplate: bool, result: &str) -> String {
    let root = match selector {
        Some(selector) => {
            let selector = serde_json::Value::from(selector).to_string();
            format!("var root=document.querySelector({selector});if(!root)return null;")
        }
        None => "var root=null;".to_string(),
    };
    format!(
        "(function(){{{root}var r={}(root,{{dropBoilerplate:{drop_boilerplate}}});{result}}})()",
        markdown_function!()
    )
}

/// JS expression that walks `document.body` and returns a markdown string
/// without the page's navigation, header, footer and sidebars. Must be
/// evaluated against a Page that has a fully-bootstrapped JS runtime.
pub const HTML_TO_MARKDOWN_JS: &str = concat!(
    markdown_function!(),
    "(null, {dropBoilerplate: true}).content\n"
);

/// [`HTML_TO_MARKDOWN_JS`] for the subtree of the first element matching
/// `selector` (`fetch --selector`), or the whole body without one. Unless
/// `drop_boilerplate`, the page's navigation, header, footer and sidebars are
/// kept. Evaluates to `null` when the selector matches nothing.
pub fn html_to_markdown_js(selector: Option<&str>, drop_boilerplate: bool) -> String {
    converter_script(selector, drop_boilerplate, "return r.content;")
}

/// The page as one JS object for `--dump json` and MCP `browser_markdown`
/// with `format: "json"`: the page-level `title`, `description`, `lang` and
/// `publishedTime` plus what [`html_to_markdown_js`] renders (`content`,
/// `links`, `images`). `links` and `images` are `[url, text]` pairs with each
/// URL once, in document order. Evaluates to `null` when the selector matches
/// nothing.
pub fn page_json_js(selector: Option<&str>, drop_boilerplate: bool) -> String {
    converter_script(
        selector,
        drop_boilerplate,
        r#"
    function metaContent(keys) {
        var found = {};
        var metas = document.getElementsByTagName('meta');
        for (var i = 0; i < metas.length; i++) {
            var content = (metas[i].getAttribute('content') || '').trim();
            if (!content) continue;
            var attrs = ['name', 'property', 'itemprop'];
            for (var a = 0; a < attrs.length; a++) {
                var key = (metas[i].getAttribute(attrs[a]) || '').trim().toLowerCase();
                if (key && !(key in found)) found[key] = content;
            }
        }
        for (var k = 0; k < keys.length; k++) if (found[keys[k]]) return found[keys[k]];
        return '';
    }
    function jsonLdDate(value, level) {
        if (!value || typeof value !== 'object' || level > 4) return '';
        if (typeof value.datePublished === 'string' && value.datePublished.trim()) return value.datePublished.trim();
        var parts = Array.isArray(value) ? value : (value['@graph'] || []);
        for (var i = 0; i < parts.length; i++) {
            var date = jsonLdDate(parts[i], level + 1);
            if (date) return date;
        }
        return '';
    }
    function publishedTime() {
        var date = metaContent(['article:published_time', 'og:article:published_time', 'datepublished',
            'date', 'dcterms.created', 'dcterms.date', 'dc.date.issued', 'dc.date', 'publishdate',
            'pubdate', 'parsely-pub-date', 'sailthru.date']);
        if (date) return date;
        var scripts = document.querySelectorAll('script[type="application/ld+json"]');
        for (var i = 0; i < scripts.length && !date; i++) {
            try { date = jsonLdDate(JSON.parse(scripts[i].textContent), 0); } catch (e) {}
        }
        if (date) return date;
        var time = document.querySelector('time[itemprop="datePublished"][datetime], time[pubdate][datetime]');
        return time ? time.getAttribute('datetime').trim() : '';
    }
    // First text per URL, a later non-empty one replacing an empty first.
    function byUrl(pairs) {
        var index = {};
        var out = [];
        for (var i = 0; i < pairs.length; i++) {
            var at = index[pairs[i][0]];
            if (at === undefined) { index[pairs[i][0]] = out.length; out.push(pairs[i]); }
            else if (!out[at][1]) out[at][1] = pairs[i][1];
        }
        return out;
    }
    var html = document.documentElement;
    return {
        title: (document.title || '').trim(),
        description: metaContent(['description', 'og:description', 'twitter:description']),
        lang: html ? (html.getAttribute('lang') || '').trim() : '',
        publishedTime: publishedTime(),
        content: r.content,
        links: byUrl(r.links),
        images: byUrl(r.images)
    };"#,
    )
}
